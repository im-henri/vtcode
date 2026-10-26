//! Whitespace contours around the visible silhouette of a source scope.
use super::*;
use std::{cmp::Reverse, collections::BinaryHeap};

type Point = (u16, u16);

#[derive(Clone, Copy)]
struct Envelope {
    left: u16,
    right: u16,
}

/// Multiline scopes include their heading. Inline scopes must exclude both
/// neighboring statements and sibling blocks on the same source line.
pub(super) fn contains(pair: Pair, line: usize, col: usize) -> bool {
    line >= pair.0.0
        && line <= pair.1.0
        && (line < pair.1.0 || col <= pair.1.1)
        && (pair.0.0 != pair.1.0 || col >= pair.0.1)
}

/// Source-owned gaps in the two rows where the boundary passes through text.
/// The route cache uses this same geometry so edits inside a heading update
/// its strokes even when the outer silhouette stays unchanged.
pub(super) fn boundary_gaps(geometry: &Geometry, pair: Pair) -> Vec<Point> {
    let scoped = |anchor: &&super::super::Anchor| contains(pair, anchor.line, anchor.col);
    let top = geometry.anchors.iter().filter(scoped).map(|a| a.y).min();
    let bottom = geometry.anchors.iter().filter(scoped).map(|a| a.y).max();
    let boundary_rows = [
        geometry.at(pair.0).and(top),
        geometry.at(pair.1).and(bottom),
    ];
    let mut gaps = Vec::new();
    let source = geometry.source_cells();
    for y in boundary_rows.into_iter().flatten() {
        let mut row = geometry
            .anchors
            .iter()
            .filter(scoped)
            .filter(|a| a.y == y && !matches!(a.ch, ' ' | '\t' | '\0'));
        let Some(first) = row.next() else { continue };
        let (left, right) = row.fold((first.x, first.x + first.width - 1), |(left, right), a| {
            (left.min(a.x), right.max(a.x + a.width - 1))
        });
        for anchor in geometry
            .anchors
            .iter()
            .filter(scoped)
            .filter(|a| a.y == y && matches!(a.ch, ' ' | '\t'))
        {
            gaps.extend(
                (anchor.x..anchor.x.saturating_add(anchor.width)).filter_map(|x| {
                    (x > left
                        && x < right
                        && !source.contains((x, y))
                        && !geometry.obstacles.contains(&(x, y)))
                    .then_some((x, y))
                }),
            );
        }
    }
    gaps.sort_unstable();
    gaps.dedup();
    gaps
}

fn direction(from: Point, to: Point) -> u8 {
    if to.0 > from.0 {
        super::super::EAST
    } else if to.0 < from.0 {
        super::super::WEST
    } else if to.1 > from.1 {
        super::super::SOUTH
    } else {
        super::super::NORTH
    }
}

/// Walk through hidden source cells as well as the visible whitespace strokes.
/// Source glyphs complete the perimeter without ever becoming paint targets.
fn append_orbit(orbit: &mut Vec<Point>, to: Point) {
    if let Some(mut from) = orbit.last().copied() {
        while from.0 != to.0 {
            from.0 = if from.0 < to.0 {
                from.0 + 1
            } else {
                from.0 - 1
            };
            orbit.push(from);
        }
        while from.1 != to.1 {
            from.1 = if from.1 < to.1 {
                from.1 + 1
            } else {
                from.1 - 1
            };
            orbit.push(from);
        }
    } else {
        orbit.push(to);
    }
}

/// Minimize exterior whitespace first, then travel and turns. A vertical edge
/// must clear both adjacent source rows; their shared exterior is the tightest
/// safe position. Monotone travel prevents doubled-back, one-row notches.
fn side_paths(
    bounds: Rect,
    envelopes: &[Envelope],
    left_side: bool,
    available: &impl Fn(u16, u16) -> bool,
) -> Vec<Vec<Point>> {
    let width = usize::from(bounds.width);
    let height = usize::from(bounds.height);
    let permitted = |x: u16, y: u16| {
        let envelope = envelopes[usize::from(y - bounds.y)];
        (if left_side {
            x < envelope.left
        } else {
            x > envelope.right
        }) && available(x, y)
    };
    let distance = |x: u16, y: u16| {
        let envelope = envelopes[usize::from(y - bounds.y)];
        u64::from(if left_side {
            envelope.left.saturating_sub(x).saturating_sub(1)
        } else {
            x.saturating_sub(envelope.right).saturating_sub(1)
        })
    };
    let index = |x: u16, y: u16, heading: usize| {
        (usize::from(y - bounds.y) * width + usize::from(x - bounds.x)) * 3 + heading
    };
    let point = |index: usize| {
        let cell = index / 3;
        (
            bounds.x + (cell % width) as u16,
            bounds.y + (cell / width) as u16,
        )
    };
    let mut paths = Vec::new();
    // Successive fragments occupy disjoint row spans, so visited states never
    // need clearing. Allocate one bounded grid per side, including when many
    // annotation or omitted rows split the visible contour.
    // Lexicographic costs keep a useful indentation change from being traded
    // away merely to save a few horizontal cells or rounded turns.
    let unreachable = (u64::MAX, u64::MAX);
    let mut costs = vec![unreachable; width * height * 3];
    let mut parents = vec![usize::MAX; costs.len()];
    let mut start_y = bounds.y;
    while start_y < bounds.bottom() {
        let starts: Vec<_> = (bounds.x..bounds.right())
            .filter(|&x| permitted(x, start_y))
            .collect();
        if starts.is_empty() {
            start_y += 1;
            continue;
        }
        let mut pending = BinaryHeap::new();
        for x in starts {
            let state = index(x, start_y, 0);
            let cost = (distance(x, start_y), 0);
            costs[state] = cost;
            parents[state] = state;
            pending.push(Reverse((cost, state)));
        }
        let mut last_y = start_y;
        let mut best = None;
        let mut best_score = unreachable;
        while let Some(Reverse((cost, state))) = pending.pop() {
            if costs[state] != cost {
                continue;
            }
            let (x, y) = point(state);
            if last_y + 1 == bounds.bottom() && cost > best_score {
                break;
            }
            let score = (cost.0 + distance(x, y), cost.1);
            if y > last_y || y == last_y && score < best_score {
                last_y = y;
                best = Some(state);
                best_score = score;
            }
            for (next, heading) in [
                (y.checked_add(1).map(|y| (x, y)), 0),
                (x.checked_sub(1).map(|x| (x, y)), 1),
                (x.checked_add(1).map(|x| (x, y)), 2),
            ] {
                let Some((nx, ny)) = next else { continue };
                if !bounds.contains((nx, ny).into()) || !permitted(nx, ny) {
                    continue;
                }
                let next_state = index(nx, ny, heading);
                let (slack, travel) = if heading == 0 {
                    // Whichever adjacent row extends farther out determines
                    // this edge. Following a shorter row cannot cross the
                    // neighboring row's text or create an overlapping turn.
                    (distance(x, y).min(distance(nx, ny)), 16)
                } else {
                    (0, 8)
                };
                let turn = if state % 3 == heading { 0 } else { 14 };
                let next_cost = (cost.0 + slack, cost.1 + travel + turn);
                if next_cost < costs[next_state] {
                    costs[next_state] = next_cost;
                    parents[next_state] = state;
                    pending.push(Reverse((next_cost, next_state)));
                }
            }
        }
        if let Some(mut state) = best {
            let mut path = Vec::new();
            loop {
                path.push(point(state));
                if parents[state] == state {
                    break;
                }
                state = parents[state];
            }
            path.reverse();
            paths.push(path);
        }
        // A fully blocked exterior row is a clip, never permission to cross
        // source text or an annotation. Resume below it when space returns.
        start_y = last_y.saturating_add(1);
    }
    paths
}

pub(super) fn route(
    painter: &Painter<'_>,
    geometry: &Geometry,
    gutter: Rect,
    rows: &BTreeMap<usize, (u16, u16)>,
    mut pair: Pair,
    tight: bool,
) -> Option<Route> {
    if pair.1 < pair.0 {
        std::mem::swap(&mut pair.0, &mut pair.1);
    }
    let first = source_endpoint(painter, geometry, rows, pair.0)?;
    let second = source_endpoint(painter, geometry, rows, pair.1)?;
    if geometry.area.is_empty() {
        return None;
    }
    let scoped = |anchor: &&super::super::Anchor| contains(pair, anchor.line, anchor.col);
    let mut source_rows = BTreeMap::<u16, Envelope>::new();
    let mut visible_top = u16::MAX;
    let mut visible_bottom = 0;
    for anchor in geometry.anchors.iter().filter(scoped) {
        visible_top = visible_top.min(anchor.y);
        visible_bottom = visible_bottom.max(anchor.y);
        if matches!(anchor.ch, ' ' | '\t' | '\0') {
            continue;
        }
        let right = anchor.x.saturating_add(anchor.width.saturating_sub(1));
        source_rows
            .entry(anchor.y)
            .and_modify(|envelope| {
                envelope.left = envelope.left.min(anchor.x);
                envelope.right = envelope.right.max(right);
            })
            .or_insert(Envelope {
                left: anchor.x,
                right,
            });
    }
    if source_rows.is_empty() || visible_top == u16::MAX {
        return None;
    }
    let top = visible_top.min(first.y.min(second.y)).max(geometry.area.y);
    let bottom = visible_bottom
        .max(first.y.max(second.y))
        .min(geometry.area.bottom().saturating_sub(1));
    let area = if gutter.is_empty() {
        geometry.area
    } else {
        geometry.area.union(gutter)
    };
    let bounds = Rect::new(area.x, top, area.width, bottom.checked_sub(top)? + 1);
    let count = usize::from(bounds.width) * usize::from(bounds.height);
    if count == 0 || count > MAX_ROUTE_CELLS {
        return None;
    }
    let source_cells = geometry.source_cells();
    let available = |x, y| {
        (geometry.area.contains((x, y).into()) || gutter.contains((x, y).into()))
            // A compact inline block may have no exterior whitespace. Do not
            // jump over a neighboring token to draw a mark beside another block.
            && (pair.0.0 != pair.1.0
                || top != bottom
                || source_rows.get(&y).is_some_and(|row| {
                    x >= row.left.saturating_sub(1) && x <= row.right.saturating_add(1)
                }))
            && !source_cells.contains((x, y))
            && !geometry.obstacles.contains(&(x, y))
            && route_cell(painter, x, y)
    };
    // Registered obstacles participate in the cache key. An unregistered
    // annotation can still occupy a rendered cell, so its detour must notice
    // opened whitespace rather than retaining the old exterior forever.
    let external_walls = (top..=bottom).any(|y| {
        (geometry.area.x..geometry.area.right()).any(|x| {
            !source_cells.contains((x, y))
                && !painter.overlays.contains(&(x, y))
                && painter
                    .buffer
                    .cell((x, y))
                    .is_some_and(|cell| cell.symbol() != " " && !rail_symbol(cell.symbol()))
        })
    });
    // Empty source rows keep a contour, using the nearest real row rather than
    // inventing a box around an end-of-line sentinel.
    let mut envelopes: Vec<_> = (top..=bottom)
        .map(|y| {
            *source_rows
                .iter()
                .min_by_key(|(row, _)| row.abs_diff(y))
                .unwrap()
                .1
        })
        .collect();
    if !tight {
        // A loose outline keeps its left rail outside every indentation and
        // closes one- and two-row right-hand recesses. Broad changes in line
        // length still shape the outline, including its endpoint rows.
        let left = envelopes.iter().map(|row| row.left).min().unwrap();
        let original = envelopes.clone();
        let right_at =
            |row: isize| original[row.clamp(0, original.len() as isize - 1) as usize].right;
        for (row, envelope) in envelopes.iter_mut().enumerate() {
            envelope.left = left;
            envelope.right = (-1..=1)
                .map(|offset| {
                    (-1..=1)
                        .map(|neighbor| right_at(row as isize + offset + neighbor))
                        .max()
                        .unwrap()
                })
                .min()
                .unwrap();
        }
    }
    let mut left_paths = side_paths(bounds, &envelopes, true, &available);
    let right_paths = side_paths(bounds, &envelopes, false, &available);
    if !tight {
        // A straight rail may sit outside an indented heading or closer. Join
        // that row's real text with one horizontal arm instead of leaving a
        // floating vertical endpoint at the global indentation.
        for opening in [true, false] {
            if geometry.at(if opening { pair.0 } else { pair.1 }).is_none() {
                continue;
            }
            let y = if opening { top } else { bottom };
            let Some(target) = source_rows.get(&y).and_then(|row| row.left.checked_sub(1)) else {
                continue;
            };
            let path = if opening {
                left_paths.first_mut()
            } else {
                left_paths.last_mut()
            };
            let Some(path) = path else { continue };
            let index = if opening {
                path.iter().rposition(|point| point.1 == y)
            } else {
                path.iter().position(|point| point.1 == y)
            };
            let Some(index) = index else { continue };
            let rail = path[index];
            if target <= rail.0 || !(rail.0..=target).all(|x| available(x, y)) {
                continue;
            }
            if opening {
                let mut joined: Vec<_> = (rail.0..=target).rev().map(|x| (x, y)).collect();
                joined.extend_from_slice(&path[index + 1..]);
                *path = joined;
            } else {
                path.truncate(index + 1);
                path.extend((rail.0 + 1..=target).map(|x| (x, y)));
            }
        }
    }
    let mut fill: Vec<_> = envelopes
        .iter()
        .enumerate()
        .map(|(index, envelope)| fill::Span {
            y: top + index as u16,
            left: i64::from(envelope.left) - 1,
            right: i64::from(envelope.right) + 1,
        })
        .collect();
    // Use the inner edge of each rounded turn, never its outer bounding box.
    for (paths, left_side) in [(&left_paths, true), (&right_paths, false)] {
        let mut edges = BTreeMap::<u16, u16>::new();
        for &(x, y) in paths.iter().flatten() {
            edges
                .entry(y)
                .and_modify(|edge| {
                    *edge = if left_side {
                        (*edge).max(x)
                    } else {
                        (*edge).min(x)
                    };
                })
                .or_insert(x);
        }
        for (y, x) in edges {
            let span = &mut fill[usize::from(y - top)];
            if left_side {
                span.left = i64::from(x);
            } else {
                span.right = i64::from(x);
            }
        }
    }
    let mut connections = BTreeMap::<Point, u8>::new();
    let mut add_path = |path: &[Point]| {
        for window in path.windows(2) {
            let [from, to] = [window[0], window[1]];
            *connections.entry(from).or_default() |= direction(from, to);
            *connections.entry(to).or_default() |= direction(to, from);
        }
        if let Some(&point) = path.first() {
            connections.entry(point).or_default();
        }
    };
    for path in left_paths.iter().chain(&right_paths) {
        add_path(path);
    }
    let opening_visible = geometry.at(pair.0).is_some();
    let closing_visible = geometry.at(pair.1).is_some();
    let complete_side = |paths: &Vec<Vec<Point>>| {
        paths.len() == 1
            && paths[0].first().is_some_and(|point| point.1 == top)
            && paths[0].last().is_some_and(|point| point.1 == bottom)
    };
    let exact = complete_side(&left_paths) && complete_side(&right_paths) && !external_walls;
    drop(add_path);
    // The source heading and closing delimiter complete the visible outline.
    // End beside their text, without adding rows above or below the scope.
    // A clipped side keeps its vertical tangent at the viewport boundary.
    for (left_side, paths) in [(true, &left_paths), (false, &right_paths)] {
        for path in paths {
            for (opening, point) in [(true, path.first()), (false, path.last())] {
                let Some(&point) = point else { continue };
                let mask = connections.entry(point).or_default();
                if mask.count_ones() >= 2 {
                    continue;
                }
                let envelope = source_rows
                    .get(&point.1)
                    .copied()
                    .unwrap_or(envelopes[usize::from(point.1 - top)]);
                let source_neighbor = if left_side {
                    point.0.checked_add(1).map(|x| (x, point.1))
                } else {
                    point.0.checked_sub(1).map(|x| (x, point.1))
                };
                let source_adjacent = source_neighbor.is_some_and(|neighbor| {
                    source_cells.contains(neighbor)
                        && (if left_side {
                            neighbor.0 == envelope.left
                        } else {
                            neighbor.0 == envelope.right
                        })
                });
                let clipped_at_edge = if opening {
                    point.1 == geometry.area.y && !opening_visible
                } else {
                    point.1 + 1 == geometry.area.bottom() && !closing_visible
                };
                if source_adjacent && !clipped_at_edge {
                    *mask |= if left_side {
                        super::super::EAST
                    } else {
                        super::super::WEST
                    };
                } else if point.1 == geometry.area.y && opening
                    || point.1 + 1 == geometry.area.bottom() && !opening
                {
                    *mask |= if opening {
                        super::super::NORTH
                    } else {
                        super::super::SOUTH
                    };
                }
            }
        }
    }
    // The boundary passes through its heading and closing text. Keep every
    // source glyph, and continue the stroke through source-owned whitespace
    // between them so words do not leave holes in that boundary.
    for (x, y) in boundary_gaps(geometry, pair) {
        if available(x, y) {
            *connections.entry((x, y)).or_default() |= super::super::EAST | super::super::WEST;
        }
    }
    // Clockwise distance gives every column its own reveal time. Crossing the
    // heading and closer virtually also keeps the orbit continuous when text
    // interrupts a boundary or annotations clip one of its visible sides.
    let endpoint = |paths: &[Vec<Point>], opening: bool, left: bool| {
        let y = if opening { top } else { bottom };
        let point = if opening {
            paths.first().and_then(|path| path.first())
        } else {
            paths.last().and_then(|path| path.last())
        };
        point
            .filter(|point| point.1 == y)
            .copied()
            .unwrap_or_else(|| {
                let envelope = envelopes[usize::from(y - top)];
                let x = if left {
                    envelope.left.saturating_sub(1)
                } else {
                    envelope.right.saturating_add(1)
                };
                (x.clamp(bounds.x, bounds.right() - 1), y)
            })
    };
    let left_top = endpoint(&left_paths, true, true);
    let right_top = endpoint(&right_paths, true, false);
    let left_bottom = endpoint(&left_paths, false, true);
    let right_bottom = endpoint(&right_paths, false, false);
    let mut orbit = Vec::new();
    append_orbit(&mut orbit, left_top);
    append_orbit(&mut orbit, right_top);
    for path in &right_paths {
        for &point in path {
            append_orbit(&mut orbit, point);
        }
    }
    append_orbit(&mut orbit, right_bottom);
    append_orbit(&mut orbit, left_bottom);
    for path in left_paths.iter().rev() {
        for &point in path.iter().rev() {
            append_orbit(&mut orbit, point);
        }
    }
    append_orbit(&mut orbit, left_top);
    if orbit.len() > 1 && orbit.last() == orbit.first() {
        orbit.pop();
    }
    let mut distances = BTreeMap::new();
    for (index, &point) in orbit.iter().enumerate() {
        distances.entry(point).or_insert(index as f32);
    }
    let length = orbit.len().max(1) as f32;
    let cells = connections
        .into_iter()
        .filter(|(_, mask)| *mask != 0)
        .map(|((x, y), mask)| RouteCell {
            x,
            y,
            symbol: super::super::scope_symbol(mask),
            cost: distances[&(x, y)],
            terminal: y <= top || y >= bottom,
        })
        .collect::<Vec<_>>();
    if cells.is_empty() && fill.is_empty() && pair.0.0 != pair.1.0 {
        return None;
    }
    Some(Route {
        cells,
        fill,
        first,
        second,
        length,
        exact,
    })
}
