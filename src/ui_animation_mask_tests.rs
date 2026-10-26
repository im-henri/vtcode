use super::*;

#[test]
fn source_occupancy_matches_all_glyph_cells_and_invalidates_with_geometry() {
    fn check(geometry: &Geometry) {
        let expected: HashSet<_> = geometry.anchors.iter()
            .filter(|a| !matches!(a.ch, ' ' | '\t' | '\0'))
            .flat_map(|a| (0..a.width).map(move |dx| (a.x + dx, a.y)))
            .collect();
        let cells = geometry.source_cells();
        assert!(std::ptr::eq(cells, geometry.source_cells()));
        for y in 0..50 {
            for x in 0..100 {
                assert_eq!(cells.contains((x, y)), expected.contains(&(x, y)), "{x},{y}");
            }
        }
    }
    let mut geometry = geometry();
    check(&geometry);
    geometry.push_diff(50, 0, '界', 10, 10, 2);
    check(&geometry);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 50));
    for anchor in &geometry.anchors {
        buffer[(anchor.x, anchor.y)].set_char(anchor.ch);
    }
    buffer[(10, 10)].set_char(' ');
    geometry.retain_validated(&buffer);
    check(&geometry);
    geometry.reset(Rect::new(10, 4, 50, 20), true);
    geometry.push_diff(0, 0, 'x', 0, 0, 1);
    check(&geometry);
    // Oversized rectangles take the sparse fallback, with identical answers.
    geometry.reset(Rect::new(0, 0, 600, 400), true);
    geometry.push_diff(0, 0, '界', 3, 8, 2);
    check(&geometry);
}

fn geometry() -> Geometry {
    let mut geometry = Geometry::new(Rect::new(3, 2, 80, 40), true);
    for line in 0..40 {
        for col in 0..76 {
            geometry.push(
                line,
                col,
                if col % 5 == 0 { ' ' } else { 'x' },
                line,
                col,
                1,
            );
        }
        geometry.push(line, 76, '界', line, 76, 2);
        geometry.push(line, 77, '\t', line, 78, 2);
    }
    geometry.protect(4, 3, 2);
    geometry.obstacle(5, 8, 4);
    geometry.record_primary_cursor(3, 76, 2, Style::default());
    geometry
}

#[test]
fn viewport_protection_has_no_duplicate_sparse_cells_and_preserves_outside_fallback() {
    for area in [Rect::new(3, 2, 80, 40), Rect::new(0, 0, 600, 400), Rect::default()] {
        let mut geometry = geometry();
        geometry.area = area;
        geometry.protected.insert((0, 0));
        let selection = Some((0, 0, 40, 0));
        let expected = reference(&geometry, None, selection, &[], &[]);
        let protection = geometry.protection(None, selection, &[], &[]);
        assert_eq!(protection.protected_cells(), expected.0);
        assert_eq!(protection.caret_cells(), expected.1);
        if protection.mask.is_some() {
            assert!(protection.protected.iter().all(|&cell| !area.contains(cell.into())));
            assert!(protection.carets.iter().all(|&cell| !area.contains(cell.into())));
        }
    }
}

fn reference(
    geometry: &Geometry,
    cursor: Option<Anchor>,
    selection: Option<(usize, usize, usize, usize)>,
    extra: &[(usize, usize)],
    anchors: &[Option<(usize, usize)>],
) -> (HashSet<(u16, u16)>, HashSet<(u16, u16)>) {
    let mut protected = geometry.protected.clone();
    protected.extend(&geometry.obstacles);
    let mut carets = HashSet::new();
    if let Some((area, _)) = geometry.primary_cursor {
        for x in area.x..area.right() {
            carets.insert((x, area.y));
            protected.insert((x, area.y));
        }
    }
    for anchor in &geometry.anchors {
        let position = (anchor.line, anchor.col);
        let caret = extra.contains(&position)
            || geometry.primary_cursor.is_some()
                && cursor.is_some_and(|c| (c.x, c.y) == (anchor.x, anchor.y));
        let extra_selected = extra.iter().zip(anchors).any(|(&end, &start)| {
            start.is_some_and(|start| position >= start.min(end) && position < start.max(end))
        });
        for dx in 0..anchor.width {
            let cell = (anchor.x + dx, anchor.y);
            if caret {
                carets.insert(cell);
            }
            if caret || selected(anchor, selection) || extra_selected {
                protected.insert(cell);
            }
        }
    }
    (protected, carets)
}

#[test]
fn cached_protection_matches_direct_selection_membership_and_tracks_changes() {
    let mut geometry = geometry();
    let extra = [(2, 6), (6, 30), (10, 8), (3, 76), (2, 6)];
    let anchors = [
        Some((4, 2)),
        Some((2, 1)),
        Some((10, 8)),
        None,
        Some((1, 0)),
    ];
    let mut previous = None;
    for selection in [None, Some((1, 3, 20, 5)), Some((3, 76, 3, 77))] {
        for extras in [&[][..], &extra[..]] {
            for extra_anchors in [&[][..], &anchors[..]] {
                for cursor in [None, geometry.at((3, 76)), geometry.at((8, 5))] {
                    let expected = reference(&geometry, cursor, selection, extras, extra_anchors);
                    let cached = geometry.protection(cursor, selection, extras, extra_anchors);
                    assert_eq!(cached.protected_cells(), expected.0);
                    assert_eq!(cached.caret_cells(), expected.1);
                    assert!(Rc::ptr_eq(
                        &cached,
                        &geometry.protection(cursor, selection, extras, extra_anchors)
                    ));
                    if let Some(previous) = previous.replace(Rc::clone(&cached)) {
                        assert!(!Rc::ptr_eq(&previous, &cached));
                    }
                }
            }
        }
    }
    // A hidden caret and a changed rendering projection cannot retain ownership.
    geometry.primary_cursor = None;
    geometry.invalidate_rows();
    let cached = geometry.protection(geometry.at((3, 76)), None, &[], &[]);
    assert!(cached.caret_cells().is_empty());
    assert!(!cached.protected_cells().contains(&(79, 5)));
}

#[test]
fn geometry_mutations_invalidate_masks_and_wide_glyph_coverage() {
    let mut geometry = geometry();
    let before = geometry.protection(None, None, &[], &[]);
    assert!(wide_cells(&geometry).contains(&(80, 2)));
    assert!(
        !wide_cells(&geometry).contains(&(81, 2)),
        "tab cells may receive effect glyphs"
    );
    assert!(std::ptr::eq(wide_cells(&geometry), wide_cells(&geometry)));
    geometry.protect(7, 8, 3);
    let after = geometry.protection(None, None, &[], &[]);
    assert!(!Rc::ptr_eq(&before, &after));
    assert!(after.protected_cells().contains(&(11, 9)));
    geometry.obstacle(8, 9, 1);
    assert!(
        geometry
            .protection(None, None, &[], &[])
            .protected_cells()
            .contains(&(12, 10))
    );
    geometry.record_primary_cursor(2, 10, 1, Style::default());
    assert!(
        geometry
            .protection(None, None, &[], &[])
            .caret_cells()
            .contains(&(13, 4))
    );
    geometry.push(0, 78, '\u{0301}', 0, 79, 1);
    assert!(
        !wide_cells(&geometry).contains(&(80, 2)),
        "ambiguous rows lose all anchors"
    );
    geometry.occlude_top(3);
    assert!(!wide_cells(&geometry).contains(&(80, 3)));
    assert!(geometry.protection(None, None, &[], &[]).caret_cells().is_empty());
}

#[test]
fn suppressed_overlays_observe_scope_without_preparing_or_painting_masks() {
    use ratatui::{Terminal, backend::TestBackend};
    let geometry = geometry();
    let mut animations = crate::animations::AnimationState::default();
    let mut settings = crate::animation_config::AnimationSettings::default();
    settings.set(Effect::ScopeConnector, true);
    let now = Instant::now();
    let mut terminal = Terminal::new(TestBackend::new(90, 45)).unwrap();
    terminal
        .draw(|frame| {
            for anchor in &geometry.anchors {
                frame.buffer_mut()[(anchor.x, anchor.y)].set_char(if anchor.ch == '\t' {
                    ' '
                } else {
                    anchor.ch
                });
            }
            let expected = frame.buffer_mut().clone();
            draw_overlays_at(
                frame,
                &geometry,
                Rect::new(0, 2, 3, 40),
                &mut animations,
                &settings,
                crate::app_theme::EditorTheme::Dark.colors(),
                OverlayOptions {
                    snapshot: Snapshot::read_only_motion(1, (3, 76), (0, 0, 0), geometry.area),
                    scope: Some(((0, 0), (39, 0))),
                    projection: None,
                    bracket_index: None,
                    colorize: false,
                    extra_cursors: &[],
                    extra_cursor_anchors: &[],
                    allow_cursor_effects: true,
                    suppressed: true,
                },
                now,
            );
            assert_eq!(frame.buffer_mut(), &expected);
        })
        .unwrap();
    assert!(geometry.protection.borrow().is_none());
    assert!(
        animations.next_frame_wait(&settings, now).is_some(),
        "scope entry was still observed"
    );
}

// Frozen pre-optimization mask preparation, kept separate from the direct
// membership oracle above so timings represent the old hash/merged-range path.
fn original_protection(
    geometry: &Geometry,
    cursor: Option<Anchor>,
    selection: Option<(usize, usize, usize, usize)>,
    extra_cursors: &[(usize, usize)],
    extra_cursor_anchors: &[Option<(usize, usize)>],
) -> (HashSet<(u16, u16)>, HashSet<(u16, u16)>) {
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
    let mut protected = geometry.protected.clone();
    protected.extend(geometry.obstacles.iter().copied());
    let mut carets = HashSet::new();
    if let Some((area, _)) = geometry.primary_cursor {
        for x in area.x..area.right() {
            carets.insert((x, area.y));
            protected.insert((x, area.y));
        }
    }
    for anchor in &geometry.anchors {
        let position = (anchor.line, anchor.col);
        let caret = extra.contains(&position)
            || geometry.primary_cursor.is_some()
                && cursor.is_some_and(|c| (c.x, c.y) == (anchor.x, anchor.y));
        let extra_selected = merged
            .get(merged.partition_point(|(_, end)| *end <= position))
            .is_some_and(|(start, end)| position >= *start && position < *end);
        for dx in 0..anchor.width {
            let cell = (anchor.x.saturating_add(dx), anchor.y);
            if caret {
                carets.insert(cell);
            }
            if caret || selected(anchor, selection) || extra_selected {
                protected.insert(cell);
            }
        }
    }
    (protected, carets)
}

#[test]
fn bounded_protection_matches_full_scan_for_wrapping_clipping_and_diff_projections() {
    for projection in 0..4 {
        let mut geometry = geometry();
        if projection == 1 {
            // Two rendered rows per source line.
            for anchor in &mut geometry.anchors {
                anchor.col += (anchor.line % 2) * 100;
                anchor.line = 100 + anchor.line / 2;
            }
        } else if projection == 2 {
            geometry.anchors.reverse();
            geometry.ordered = false;
        } else if projection == 3 {
            geometry.diff_projection = Some((1, true));
        }
        for selection in [None, Some((3, 5, 3, 20)), Some((1, 0, 38, 0)), Some((120, 0, 121, 0)), Some((100, 5, 103, 110)), Some((8, 0, 3, 0))] {
            for cursor in [None, geometry.anchors.first().copied(), geometry.anchors.get(600).copied()] {
                for extras in [&[][..], &[(2, 6), (103, 107), (200, 0)][..]] {
                    let anchors = [Some((5, 5)), Some((100, 5)), None];
                    let expected = reference(&geometry, cursor, selection, extras, &anchors);
                    let actual = geometry.protection(cursor, selection, extras, &anchors);
                    assert_eq!((actual.protected_cells(), actual.caret_cells()), expected, "projection={projection}, selection={selection:?}, cursor={cursor:?}");
                }
            }
        }
    }
}

#[test]
#[ignore = "comparative release benchmark"]
fn benchmark_editor_animation_protection_bounds() {
    use std::hint::black_box;
    let mut geometry = Geometry::new(Rect::new(0, 0, 180, 60), true);
    for line in 0..60 {
        for col in 0..180 { geometry.push(line, col, 'x', line, col, 1); }
    }
    geometry.record_primary_cursor(30, 10, 1, Style::default());
    let cursor = geometry.at((30, 10));
    for selection in [None, Some((30, 3, 30, 75)), Some((28, 3, 33, 15))] {
        let mut old = Vec::new(); let mut new = Vec::new();
        for round in 0..9 {
            for optimized in [round % 2 == 0, round % 2 != 0] {
                geometry.ordered = optimized;
                let start = Instant::now();
                for _ in 0..1_000 {
                    geometry.clear_protection_cache();
                    black_box(geometry.protection(black_box(cursor), black_box(selection), &[], &[]));
                }
                let elapsed = start.elapsed() / 1_000;
                if optimized { new.push(elapsed); } else { old.push(elapsed); }
            }
        }
        old.sort_unstable(); new.sort_unstable();
        eprintln!("animation protection {selection:?}: full scan {:?}, bounded {:?}, {:.2}x", old[4], new[4], old[4].as_secs_f64() / new[4].as_secs_f64());
    }
}

#[test]
#[ignore = "comparative release microbenchmark"]
fn benchmark_animation_protection_reuse() {
    use std::hint::black_box;
    let geometry = geometry();
    let cursor = geometry.at((3, 76));
    let selection = Some((1, 0, 30, 70));
    let extras = [(10, 20), (20, 30)];
    let anchors = [Some((5, 10)), Some((30, 5))];
    let expected = original_protection(&geometry, cursor, selection, &extras, &anchors);
    let retained = geometry.protection(cursor, selection, &extras, &anchors);
    assert_eq!(expected.0, retained.protected_cells());
    assert_eq!(expected.1, retained.caret_cells());
    let iterations = 2_000;
    let mut samples = [Vec::new(), Vec::new()];
    for round in 0..7 {
        for cached in [round % 2 == 0, round % 2 != 0] {
            for _ in 0..100 {
                if cached {
                    black_box(geometry.protection(cursor, selection, &extras, &anchors));
                } else {
                    black_box(original_protection(
                        &geometry, cursor, selection, &extras, &anchors,
                    ));
                }
            }
            let start = Instant::now();
            for _ in 0..iterations {
                if cached {
                    black_box(geometry.protection(black_box(cursor), selection, &extras, &anchors));
                } else {
                    black_box(original_protection(
                        black_box(&geometry),
                        cursor,
                        selection,
                        &extras,
                        &anchors,
                    ));
                }
            }
            samples[usize::from(cached)].push(start.elapsed() / iterations);
        }
    }
    for samples in &mut samples {
        samples.sort_unstable();
    }
    eprintln!(
        "animation protection per frame median: original {:?}, retained {:?}",
        samples[0][3], samples[1][3]
    );
}
