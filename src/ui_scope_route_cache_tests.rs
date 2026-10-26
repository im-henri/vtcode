//! Small routing fixtures isolate reuse from editor layout and animation state.
use super::*;

use ratatui::buffer::Buffer;
use std::{collections::HashSet, hint::black_box, time::Duration};

const AREA: Rect = Rect::new(4, 1, 28, 10);
const GUTTER: Rect = Rect::new(0, 1, 3, 10);
const PAIR: Pair = ((0, 4), (3, 4));
const SOURCE: &[&str] = &["    {", "        work();", "        finish();", "    }"];

fn fixture(text: &[&str], area: Rect) -> (Buffer, Geometry) {
    let mut buffer = Buffer::empty(Rect::new(0, 0, 120, 40));
    let mut geometry = Geometry::new(area, true);
    for (row, line) in text.iter().enumerate() {
        let mut column = 0;
        for (col, ch) in line.chars().enumerate() {
            let width = unicode_width::UnicodeWidthChar::width(ch)
                .unwrap_or(0)
                .max(1);
            geometry.push(row, col, ch, row, column, width);
            buffer[(area.x + column as u16, area.y + row as u16)].set_symbol(&ch.to_string());
            column += width;
        }
        geometry.push(row, line.chars().count(), '\0', row, column, 1);
    }
    (buffer, geometry)
}

fn painter<'a>(buffer: &'a mut Buffer, geometry: &Geometry) -> Painter<'a> {
    Painter::new(
        buffer,
        geometry.area,
        crate::app_theme::EditorTheme::Dark.colors(),
        HashSet::new(),
        HashSet::new(),
        geometry.unsafe_rows.clone(),
    )
}

fn points(route: &Route) -> Vec<(u16, u16)> {
    route.cells.iter().map(|cell| (cell.x, cell.y)).collect()
}

#[test]
fn retained_walls_reuse_only_unchanged_projection_and_bounds() {
    let (mut buffer, mut geometry) = fixture(SOURCE, AREA);
    geometry.retain_validated(&buffer);
    let mut cache = RouteCache::default();
    let mut original = Vec::new();
    for frame in 0..4 {
        let painter = painter(&mut buffer, &geometry);
        cache.prepare(&painter, &geometry, GUTTER);
        if frame == 0 { original = cache.walls.clone(); }
        assert_eq!(cache.walls, original);
        assert_eq!(cache.wall_builds, 1);
    }
    geometry.obstacle(5, 8, 1);
    geometry.retain_validated(&buffer);
    let painter = painter(&mut buffer, &geometry);
    cache.prepare(&painter, &geometry, GUTTER);
    assert_ne!(cache.walls, original);
    assert_eq!(cache.wall_builds, 2);
    cache.prepare_rows(&painter, &geometry, GUTTER, AREA.y + 2, AREA.bottom() - 1);
    assert_eq!(cache.wall_builds, 3);
}

#[test]
fn retained_route_keys_reuse_geometry_but_recheck_hints_and_obstructions() {
    let (mut buffer, mut geometry) = fixture(SOURCE, AREA);
    geometry.retain_validated(&buffer);
    let mut painter = painter(&mut buffer, &geometry);
    let mut cache = RouteCache::default();
    let mut original = Vec::new();
    for frame in 0..10 {
        cache.prepare(&painter, &geometry, GUTTER);
        let route = cache.route(&painter, &geometry, GUTTER, rows(&geometry), PAIR).unwrap();
        if frame == 0 { original = points(route); }
        assert_eq!(points(route), original);
    }
    assert_eq!(cache.key_builds, 1);
    assert_eq!(cache.builds, 1);
    let blocked = original[original.len() / 2];
    painter.buffer[blocked].set_symbol("x");
    cache.prepare(&painter, &geometry, GUTTER);
    if let Some(route) = cache.route(&painter, &geometry, GUTTER, rows(&geometry), PAIR) {
        assert!(!points(route).contains(&blocked));
    }
    assert_eq!(cache.builds, 2);
    painter.scope_endpoints.insert(PAIR.0, None);
    cache.prepare(&painter, &geometry, GUTTER);
    assert!(cache.route(&painter, &geometry, GUTTER, rows(&geometry), PAIR).is_none());
    assert!(cache.key_builds >= 2);
}

#[test]
fn exact_routes_reuse_unaffected_whitespace_after_other_cells_change() {
    let (mut buffer, geometry) = fixture(SOURCE, AREA);
    let painter = painter(&mut buffer, &geometry);
    let row_map = rows(&geometry).clone();
    let mut cache = RouteCache::default();
    cache.prepare(&painter, &geometry, GUTTER);
    let route = cache
        .route(&painter, &geometry, GUTTER, &row_map, PAIR)
        .unwrap();
    assert!(route.exact);
    let original = points(route);
    assert_eq!(cache.grid_builds, 0, "exact paths do not scan the viewport");
    // This cell changes passability without touching either attachment or
    // any route cell. Validating the retained path must avoid another search.
    let elsewhere = (AREA.right() - 1, AREA.y + 1);
    assert!(!original.contains(&elsewhere));
    painter.buffer[elsewhere].set_symbol("x");
    cache.prepare(&painter, &geometry, GUTTER);
    assert_eq!(
        points(
            cache
                .route(&painter, &geometry, GUTTER, &row_map, PAIR)
                .unwrap()
        ),
        original
    );
    assert_eq!(cache.builds, 1);

    // A same-width edit to an occupied cell does not change passability at all.
    painter.buffer[elsewhere].set_symbol("y");
    cache.prepare(&painter, &geometry, GUTTER);
    cache.route(&painter, &geometry, GUTTER, &row_map, PAIR);
    assert_eq!(cache.builds, 1);
    assert_eq!(
        cache.grid_builds, 0,
        "validating exact paths only checks their cells"
    );
}

#[test]
fn scope_terminal_rows_reuse_the_exact_contour_without_viewport_snapshots() {
    let text = ["", SOURCE[0], SOURCE[1], SOURCE[2], SOURCE[3], ""];
    let pair = ((1, 4), (4, 4));
    let (mut buffer, geometry) = fixture(&text, AREA);
    let painter = painter(&mut buffer, &geometry);
    let row_map = rows(&geometry);
    let mut cache = RouteCache::default();
    cache.prepare(&painter, &geometry, GUTTER);
    let route = cache
        .route(&painter, &geometry, GUTTER, &row_map, pair)
        .unwrap();
    assert!(route.exact, "both sides of the scope are unobstructed");
    assert!(route.cells.iter().any(|cell| cell.y == AREA.y + 1));
    assert!(route.cells.iter().any(|cell| cell.y == AREA.y + 4));
    assert!(
        route
            .cells
            .iter()
            .all(|cell| (AREA.y + 1..=AREA.y + 4).contains(&cell.y)),
        "rows outside the scope must stay unchanged"
    );
    let original = points(route);
    for _ in 0..8 {
        cache.prepare(&painter, &geometry, GUTTER);
        assert_eq!(
            points(
                cache
                    .route(&painter, &geometry, GUTTER, &row_map, pair)
                    .unwrap()
            ),
            original
        );
    }
    assert_eq!(cache.builds, 1);
    assert_eq!(cache.grid_builds, 0);
}

#[test]
fn wrapped_heading_prefix_rebuilds_when_a_blank_obstacle_clears_before_the_attachment() {
    let text = [
        "    heading_piece",
        "    heading_piece",
        "    heading_piece",
        "    heading_piece",
        "    {",
        "        body();",
        "    }",
    ];
    let (mut buffer, mut geometry) = fixture(&text, AREA);
    geometry.set_wrapped(32);
    // Project one logical heading onto five screen rows. Its attachment is
    // in the final wrapped slice, several rows below its visible prefix.
    for anchor in &mut geometry.anchors {
        let row = usize::from(anchor.y - AREA.y);
        if row < 5 {
            anchor.line = 0;
            anchor.col += row * 32;
        } else {
            anchor.line = row - 4;
        }
    }
    let pair = ((0, 4 * 32 + 4), (2, 4));
    let mut painter = painter(&mut buffer, &geometry);
    painter.scope_endpoints.insert(
        pair.0,
        Some(super::super::super::ScopeEndpointHint {
            attachment: pair.0,
            x: AREA.x + 4,
            precise: false,
            delimiter_x: Some(AREA.x + 4),
        }),
    );
    let row_map = rows(&geometry).clone();
    let unobstructed = scope_route(&painter, &geometry, GUTTER, &row_map, pair).unwrap();
    let wall = unobstructed
        .cells
        .iter()
        .find(|cell| cell.y == AREA.y + 1 && cell.x < AREA.x + 4)
        .map(|cell| (cell.x, cell.y))
        .unwrap();
    assert!(wall.1 < unobstructed.first.y.saturating_sub(ROUTE_ROW_MARGIN));
    assert_eq!(painter.buffer[wall].symbol(), " ");
    geometry.obstacles.insert(wall);

    let mut cache = RouteCache::default();
    cache.prepare(&painter, &geometry, GUTTER);
    let detour = cache
        .route(&painter, &geometry, GUTTER, &row_map, pair)
        .unwrap();
    assert!(
        detour.exact,
        "a blank obstacle still permits complete sides"
    );
    let detour = points(detour);
    assert!(!detour.contains(&wall));
    assert_eq!(cache.builds, 1);
    assert_eq!(cache.grid_builds, 0);

    geometry.obstacles.remove(&wall);
    cache.prepare(&painter, &geometry, GUTTER);
    assert!(
        detour.iter().all(|&(x, y)| cache.available(&painter, x, y)),
        "checking only retained route cells cannot detect the opened shortcut"
    );
    let updated = cache
        .route(&painter, &geometry, GUTTER, &row_map, pair)
        .unwrap();
    assert_eq!(points(updated), points(&unobstructed));
    assert_eq!(
        cache.builds, 2,
        "the prefix obstacle belongs in the route key"
    );
    assert_eq!(
        cache.grid_builds, 0,
        "complete contours retain cheap validation"
    );
}

#[test]
fn failed_routes_are_cached_until_passability_changes() {
    let (mut buffer, geometry) = fixture(SOURCE, AREA);
    let mut painter = painter(&mut buffer, &geometry);
    painter.blocked_rows.to_mut().extend(AREA.y..AREA.bottom());
    let row_map = rows(&geometry);
    let mut cache = RouteCache::default();
    for _ in 0..8 {
        cache.prepare(&painter, &geometry, GUTTER);
        assert!(
            cache
                .route(&painter, &geometry, GUTTER, &row_map, PAIR)
                .is_none()
        );
        assert!(
            cache
                .route(&painter, &geometry, GUTTER, &row_map, PAIR)
                .is_none(),
            "another query in the same frame shares the occupancy snapshot"
        );
    }
    assert_eq!(
        cache.builds, 1,
        "an impossible search must not repeat every frame"
    );
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(
        cache.grid_builds, 8,
        "failed routes scan once per prepared frame"
    );

    // Failed searches cannot retain the old verdict after a wall disappears.
    painter.blocked_rows.to_mut().clear();
    cache.prepare(&painter, &geometry, GUTTER);
    assert!(
        cache
            .route(&painter, &geometry, GUTTER, &row_map, PAIR)
            .unwrap()
            .exact
    );
    assert_eq!(cache.builds, 2);
    assert_eq!(
        cache.grid_builds, 9,
        "the new snapshot discovers the opened route"
    );
}

#[test]
fn fallback_routes_recheck_after_passability_changes() {
    let text = [
        "            previous_work();",
        "            } else {xxxxxxxx",
        "            xxxxxxxxxxxxxxxx",
        "            xxxxxxxxxxxxxxxx",
        "            xxxxxxxxxxxxxxxx",
        "        }",
    ];
    let pair = ((1, 19), (5, 8));
    let (mut buffer, geometry) = fixture(&text, AREA);
    let painter = painter(&mut buffer, &geometry);
    let row_map = rows(&geometry);
    let mut cache = RouteCache::default();
    for _ in 0..8 {
        cache.prepare(&painter, &geometry, GUTTER);
        assert!(
            !cache
                .route(&painter, &geometry, GUTTER, &row_map, pair)
                .unwrap()
                .exact
        );
    }
    assert_eq!(cache.builds, 1);
    assert_eq!(cache.grid_builds, 8);
    let original = points(
        cache
            .route(&painter, &geometry, GUTTER, &row_map, pair)
            .unwrap(),
    );

    // Newly opened space changes the silhouette even when all old contour
    // cells remain free. The opening row still reaches the right viewport edge.
    for anchor in &geometry.anchors {
        if (2..=4).contains(&anchor.line) && anchor.col >= 12 {
            painter.buffer[(anchor.x, anchor.y)].set_symbol(" ");
        }
    }
    let mut geometry = geometry;
    geometry
        .anchors
        .retain(|anchor| !(2..=4).contains(&anchor.line) || anchor.col < 12);
    cache.prepare(&painter, &geometry, GUTTER);
    let updated = cache
        .route(&painter, &geometry, GUTTER, &rows(&geometry), pair)
        .unwrap();
    assert!(!updated.exact, "the clipped opening row stays partial");
    assert_ne!(
        points(updated),
        original,
        "the bubble must follow the newly available silhouette"
    );
    assert_eq!(
        cache.builds, 2,
        "a fallback must discover newly reachable whitespace"
    );
    assert_eq!(cache.grid_builds, 9);
}

#[test]
fn reduced_row_domain_ignores_outside_edits_and_refreshes_when_extended() {
    let text = ["    if ready", "        work();", "    endif"];
    let pair = ((0, 4), (2, 4));
    let (mut buffer, geometry) = fixture(&text, AREA);
    let painter = painter(&mut buffer, &geometry);
    // Keep this a partial contour so occupancy snapshots exercise the reduced
    // domain. Block the right side of its interior row, preserving the left.
    let body_end = AREA.x + text[1].chars().count() as u16;
    for x in body_end..AREA.right() {
        painter.buffer[(x, AREA.y + 1)].set_symbol("X");
    }
    let row_map = rows(&geometry);
    let mut cache = RouteCache::default();
    cache.prepare_rows(&painter, &geometry, GUTTER, AREA.y, AREA.y + 2);
    assert!(
        !cache
            .route(&painter, &geometry, GUTTER, &row_map, pair)
            .unwrap()
            .exact
    );
    assert_eq!(cache.bounds.height, 3);
    let generation = cache.generation;

    painter.buffer[(AREA.right() - 1, AREA.y + 8)].set_symbol("X");
    cache.prepare_rows(&painter, &geometry, GUTTER, AREA.y, AREA.y + 2);
    cache.route(&painter, &geometry, GUTTER, &row_map, pair);
    assert_eq!(
        cache.generation, generation,
        "off-domain edits do not affect reachability"
    );
    assert_eq!(cache.builds, 1);
    assert_eq!(cache.grid_builds, 2);

    cache.prepare_rows(&painter, &geometry, GUTTER, AREA.y, AREA.y + 3);
    assert!(
        cache.entries.is_empty(),
        "a changed routing domain discards old projections"
    );
    cache.route(&painter, &geometry, GUTTER, &row_map, pair);
    assert_eq!(cache.builds, 2);
    assert_eq!(cache.grid_builds, 3);
}

#[test]
fn literal_box_drawing_source_invalidates_an_otherwise_passable_cached_cell() {
    let (mut buffer, mut geometry) = fixture(SOURCE, AREA);
    let painter = painter(&mut buffer, &geometry);
    let mut cache = RouteCache::default();
    cache.prepare(&painter, &geometry, GUTTER);
    let route = cache
        .route(&painter, &geometry, GUTTER, &rows(&geometry), PAIR)
        .unwrap();
    let cell = route
        .cells
        .iter()
        .find(|cell| cell.y == AREA.y + 1)
        .unwrap();
    let wall = (cell.x, cell.y);
    painter.buffer[wall].set_symbol("│");
    let anchor = geometry
        .anchors
        .iter_mut()
        .find(|anchor| (anchor.x, anchor.y) == wall)
        .unwrap();
    assert_eq!(anchor.ch, ' ');
    anchor.ch = '│';
    assert!(
        route_cell(&painter, wall.0, wall.1),
        "the terminal glyph alone resembles a guide"
    );

    cache.prepare(&painter, &geometry, GUTTER);
    assert!(
        !cache.available(&painter, wall.0, wall.1),
        "literal source is always a wall"
    );
    let updated = cache
        .route(&painter, &geometry, GUTTER, &rows(&geometry), PAIR)
        .unwrap();
    assert!(!points(updated).contains(&wall));
    assert_eq!(cache.builds, 2);
}

#[test]
fn skipped_cells_wide_continuations_obstacles_and_unsafe_rows_invalidate_routes() {
    for kind in [
        "skip",
        "wide buffer",
        "wide source",
        "obstacle",
        "unsafe row",
    ] {
        let (mut buffer, mut geometry) = fixture(SOURCE, AREA);
        let mut painter = painter(&mut buffer, &geometry);
        let mut cache = RouteCache::default();
        cache.prepare(&painter, &geometry, GUTTER);
        let route = cache
            .route(&painter, &geometry, GUTTER, &rows(&geometry), PAIR)
            .unwrap();
        let cell = route
            .cells
            .iter()
            .find(|cell| cell.y == AREA.y + 1)
            .unwrap();
        let wall = (cell.x, cell.y);
        match kind {
            "skip" => painter.buffer[wall].skip = true,
            "wide buffer" => {
                // Some renderers leave a blank continuation rather than skip.
                painter.buffer[(wall.0 - 1, wall.1)].set_symbol("界");
                assert_eq!(painter.buffer[wall].symbol(), " ");
            }
            "wide source" => {
                // Source metadata also protects the whole wide glyph when its
                // buffer cells are temporarily blanked by an overlay.
                geometry.push(1, 80, '界', 1, usize::from(wall.0 - AREA.x - 1), 2);
            }
            "obstacle" => {
                geometry.obstacles.insert(wall);
            }
            "unsafe row" => {
                geometry.push(1, 80, '\u{0301}', 1, 0, 1);
                assert!(geometry.unsafe_rows.contains(&wall.1));
                painter.blocked_rows = std::borrow::Cow::Borrowed(&geometry.unsafe_rows);
            }
            _ => unreachable!(),
        }
        cache.prepare(&painter, &geometry, GUTTER);
        assert!(
            !cache.available(&painter, wall.0, wall.1),
            "{kind} must block the cached cell"
        );
        let updated = cache.route(&painter, &geometry, GUTTER, &rows(&geometry), PAIR);
        if kind == "unsafe row" {
            let updated = updated.expect("safe rows retain their partial contours");
            assert!(
                updated.cells.iter().all(|cell| cell.y != wall.1),
                "an ambiguous row cannot be crossed even in the gutter"
            );
        } else {
            assert!(
                !points(updated.unwrap()).contains(&wall),
                "{kind} kept an obstructed path"
            );
        }
        assert_eq!(cache.builds, 2, "{kind} needs one replacement search");
    }
}

#[test]
fn moving_the_code_area_or_gutter_discards_projected_routes() {
    let (mut buffer, geometry) = fixture(SOURCE, AREA);
    let painter = painter(&mut buffer, &geometry);
    let mut cache = RouteCache::default();
    cache.prepare(&painter, &geometry, GUTTER);
    let first = cache
        .route(&painter, &geometry, GUTTER, &rows(&geometry), PAIR)
        .unwrap()
        .first;

    let moved_area = Rect::new(AREA.x + 1, AREA.y + 1, AREA.width, AREA.height);
    let moved_gutter = Rect::new(GUTTER.x + 1, GUTTER.y + 1, GUTTER.width, GUTTER.height);
    let (mut moved_buffer, moved_geometry) = fixture(SOURCE, moved_area);
    let moved_painter = self::painter(&mut moved_buffer, &moved_geometry);
    cache.prepare(&moved_painter, &moved_geometry, moved_gutter);
    assert!(cache.entries.is_empty());
    let moved = cache
        .route(
            &moved_painter,
            &moved_geometry,
            moved_gutter,
            &rows(&moved_geometry),
            PAIR,
        )
        .unwrap();
    assert_eq!((moved.first.x, moved.first.y), (first.x + 1, first.y + 1));
    assert_eq!(cache.builds, 2);

    let wider_gutter = Rect::new(0, moved_gutter.y, 4, moved_gutter.height);
    cache.prepare(&moved_painter, &moved_geometry, wider_gutter);
    assert!(
        cache.entries.is_empty(),
        "gutter geometry is part of the route's coordinate system"
    );
    cache.route(
        &moved_painter,
        &moved_geometry,
        wider_gutter,
        &rows(&moved_geometry),
        PAIR,
    );
    assert_eq!(cache.builds, 3);
}

#[test]
fn routing_history_retains_at_most_six_entries() {
    let text = ["    { { { { { { { {", "", "    } } } } } } } }"];
    let (mut buffer, geometry) = fixture(&text, AREA);
    let painter = painter(&mut buffer, &geometry);
    let mut cache = RouteCache::default();
    cache.prepare(&painter, &geometry, GUTTER);
    for index in 0..8 {
        let column = 4 + index * 2;
        assert!(
            cache
                .route(
                    &painter,
                    &geometry,
                    GUTTER,
                    &rows(&geometry),
                    ((0, column), (2, column))
                )
                .is_some()
        );
        assert!(cache.entries.len() <= 6);
    }
    assert_eq!(cache.entries.len(), 6);
    assert_eq!(cache.builds, 8);
    cache.route(
        &painter,
        &geometry,
        GUTTER,
        &rows(&geometry),
        ((0, 18), (2, 18)),
    );
    assert_eq!(cache.builds, 8, "a retained predecessor is reused");
    cache.route(
        &painter,
        &geometry,
        GUTTER,
        &rows(&geometry),
        ((0, 4), (2, 4)),
    );
    assert_eq!(cache.builds, 9, "the oldest discarded route is rebuilt");
    assert_eq!(cache.entries.len(), 6);
}

#[test]
#[ignore = "manual routing timing; run with --ignored --nocapture"]
fn benchmark_scope_routes_with_and_without_retained_geometry() {
    const CALLS: usize = 100;
    const BATCHES: usize = 5;
    let mut text = vec!["        body(value);"];
    text.resize(39, "        body(value);");
    text[0] = "    if ready {";
    text[38] = "    }";
    let area = Rect::new(4, 1, 116, 39);
    let gutter = Rect::new(0, 1, 3, 39);
    let pair = ((0, 13), (38, 4));
    let (mut buffer, geometry) = fixture(&text, area);
    let painter = painter(&mut buffer, &geometry);
    let row_map = rows(&geometry);
    let mut cache = RouteCache::default();
    cache.prepare(&painter, &geometry, gutter);
    assert!(
        cache
            .route(&painter, &geometry, gutter, &row_map, pair)
            .unwrap()
            .exact
    );
    let mut cold = Vec::<Duration>::new();
    let mut retained = Vec::<Duration>::new();
    for batch in 0..BATCHES {
        // Alternate ordering to avoid always measuring the same path first.
        for cached in [batch % 2 == 0, batch % 2 != 0] {
            let start = Instant::now();
            for _ in 0..CALLS {
                if cached {
                    cache.prepare(black_box(&painter), black_box(&geometry), gutter);
                    black_box(
                        cache
                            .route(&painter, &geometry, gutter, &row_map, pair)
                            .unwrap()
                            .cells
                            .len(),
                    );
                } else {
                    black_box(
                        scope_route(
                            black_box(&painter),
                            black_box(&geometry),
                            gutter,
                            black_box(&row_map),
                            pair,
                        )
                        .unwrap(),
                    );
                }
            }
            if cached {
                retained.push(start.elapsed());
            } else {
                cold.push(start.elapsed());
            }
        }
    }
    cold.sort_unstable();
    retained.sort_unstable();
    assert_eq!(
        cache.builds, 1,
        "all 500 retained frames share one route search"
    );
    assert_eq!(
        cache.grid_builds, 0,
        "exact paths never require viewport snapshots"
    );
    println!(
        "120x40 scope routing median of {BATCHES} batches x {CALLS} calls: direct {:?}, prepare + retained {:?}, {:.2}x speedup; cached searches {} vs {} direct searches",
        cold[BATCHES / 2],
        retained[BATCHES / 2],
        cold[BATCHES / 2].as_secs_f64() / retained[BATCHES / 2].as_secs_f64(),
        cache.builds,
        BATCHES * CALLS,
    );
}
