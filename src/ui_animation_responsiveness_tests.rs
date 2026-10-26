use super::*;
use crate::app_theme::EditorTheme;
use std::{hint::black_box, time::Instant};

fn geometry_fixture() -> (ropey::Rope, Geometry, Buffer) {
    let area = Rect::new(0, 0, 100, 45);
    let text = "    value = process(argument);\n".repeat(45);
    let rope = ropey::Rope::from_str(&text);
    let mut geometry = Geometry::new(area, true);
    let mut buffer = Buffer::empty(area);
    for (line, text) in text.lines().enumerate() {
        for (col, ch) in text.chars().enumerate() {
            geometry.push(line, col, ch, line, col, 1);
            buffer[(col as u16, line as u16)].set_char(ch);
        }
    }
    (rope, geometry, buffer)
}

#[test]
fn retained_geometry_validates_once_and_rebuilds_after_projection_mutations() {
    let (rope, mut geometry, mut buffer) = geometry_fixture();
    buffer[(4, 1)].set_symbol("annotation");
    geometry.retain_validated(&buffer);
    assert!(geometry.is_retained());
    assert!(geometry.at((1, 4)).is_none());
    let offsets = geometry.source_offsets(&rope);
    assert!(Rc::ptr_eq(&offsets, &geometry.source_offsets(&rope)));
    let projection = geometry.projection_id;
    geometry.push(46, 0, 'x', 44, 90, 1);
    assert!(!geometry.is_retained());
    assert_ne!(geometry.projection_id, projection);
    // The extra anchor is not real source, so validation must remove it.
    geometry.retain_validated(&buffer);
    assert!(geometry.at((46, 0)).is_none());
    assert!(!Rc::ptr_eq(&offsets, &geometry.source_offsets(&rope)));
    let projection = geometry.projection_id;
    geometry.obstacle(0, 50, 2);
    assert!(!geometry.is_retained());
    assert_ne!(geometry.projection_id, projection);
    geometry.retain_validated(&buffer);
    geometry.occlude_top(3);
    assert!(!geometry.is_retained());
    assert!(geometry.anchors.iter().all(|anchor| anchor.y >= 3));
}

#[test]
fn owned_geometry_validation_preserves_projection_and_recycles_storage_when_clipped() {
    let (rope, mut geometry, mut buffer) = geometry_fixture();
    geometry.scope_rows.extend([(0, 0, 0), (1, 0, 1), (40, 0, 80)]);
    geometry.overlay(2, 2, 5);
    geometry.obstacle(3, 3, 5);
    let offsets = geometry.source_offsets(&rope);
    geometry.protection(None, Some((0, 0, 3, 8)), &[], &[]);
    wide_cells(&geometry);
    buffer[(4, 1)].set_symbol("annotation");
    buffer[(10, 2)].set_symbol(" ");
    let expected = geometry.validated(&buffer).into_owned();
    let anchors = geometry.anchors.as_ptr();
    let rows = geometry.scope_rows.as_ptr();
    geometry.retain_validated(&buffer);
    let summarize = |geometry: &Geometry| geometry.anchors.iter().map(|a| (a.line, a.col, a.ch, a.x, a.y, a.width)).collect::<Vec<_>>();
    assert_eq!(summarize(&geometry), summarize(&expected));
    assert_eq!(geometry.scope_rows, expected.scope_rows);
    assert_eq!(geometry.overlays, expected.overlays);
    assert_eq!(geometry.obstacles, expected.obstacles);
    assert_eq!(geometry.anchors.as_ptr(), anchors);
    assert_eq!(geometry.scope_rows.as_ptr(), rows);
    assert!(geometry.protection.borrow().is_none() && geometry.wide_cells.get().is_none());
    assert!(!Rc::ptr_eq(&offsets, &geometry.source_offsets(&rope)));
    let offsets = geometry.source_offsets(&rope);
    geometry.retain_validated(&buffer);
    assert!(Rc::ptr_eq(&offsets, &geometry.source_offsets(&rope)));
}

#[test]
#[ignore = "manual comparison of clipped animation geometry validation"]
fn benchmark_clipped_geometry_validation() {
    let (_, base, mut buffer) = geometry_fixture();
    buffer[(4, 1)].set_symbol("annotation");
    let mut times = [Vec::new(), Vec::new()];
    for sample in 0..9 {
        for index in [sample % 2, (sample + 1) % 2] {
            let mut elapsed = std::time::Duration::ZERO;
            for _ in 0..500 {
                let mut geometry = base.clone();
                let started = Instant::now();
                if index == 0 {
                    if let Cow::Owned(validated) = geometry.validated(black_box(&buffer)) {
                        geometry = validated;
                    }
                    geometry.retained = true;
                } else {
                    geometry.retain_validated(black_box(&buffer));
                }
                elapsed += started.elapsed();
                black_box(geometry);
            }
            times[index].push(elapsed / 500);
        }
    }
    for times in &mut times { times.sort_unstable(); }
    eprintln!("clipped animation geometry: reference={:?}, in-place={:?}", times[0][4], times[1][4]);
}

#[test]
fn whitespace_offsets_follow_source_identity_and_keep_source_order() {
    let (mut rope, geometry, _) = geometry_fixture();
    let before = geometry.source_offsets(&rope);
    assert!(before.windows(2).all(|pair| pair[0].0 < pair[1].0));
    rope.insert(0, "more ");
    let after = geometry.source_offsets(&rope);
    assert!(!Rc::ptr_eq(&before, &after));
    for (offset, anchor) in after.iter() {
        assert_eq!(*offset, rope.line_to_char(anchor.line) + anchor.col);
    }
}

#[test]
fn dense_layers_and_cached_masks_match_sparse_painting_with_clipping_and_selections() {
    let (_, mut geometry, base) = geometry_fixture();
    geometry.unsafe_rows.insert(8);
    geometry.overlays.extend([(10, 5), (20, 6), (30, 10)]);
    geometry.protected.insert((9, 3));
    geometry.primary_cursor = Some((Rect::new(12, 6, 2, 1), Style::default()));
    let colors = EditorTheme::Dark.colors();
    let mut scratch = PaintScratch::default();
    for selection in [None, Some((1, 5, 12, 20)), Some((0, 0, 44, 40))] {
        let protection = geometry.protection(None, selection, &[(5, 5)], &[Some((3, 3))]);
        let protected = protection.protected_cells();
        let carets = protection.caret_cells();
        for clip in [base.area, Rect::new(3, 2, 21, 17)] {
            let mut expected = Buffer::empty(clip);
            let mut actual = expected.clone();
            for (buffer, optimized) in [(&mut expected, false), (&mut actual, true)] {
                let mut painter = Painter::new(buffer, geometry.area, colors,
                    protected.clone(), carets.clone(), geometry.unsafe_rows.clone());
                painter.overlays = Cow::Borrowed(&geometry.overlays);
                if optimized {
                    painter.mask = protection.mask.as_ref();
                    painter.take_scratch(&mut scratch);
                }
                for y in 0..47 {
                    for x in 0..105 {
                        assert_eq!(painter.is_caret((x, y)), carets.contains(&(x, y)));
                        assert_eq!(painter.is_protected((x, y)), protected.contains(&(x, y)));
                        painter.tint_to(x, y, colors.fg_accent, 0.4);
                        painter.symbol_to(x, y, "·", colors.fg_text, 0.1);
                        painter.scope_symbol_to((x, y), "─", colors.fg_accent, 0.3, false);
                        painter.scope_symbol_to((x, y), "│", colors.fg_text, 0.3, true);
                    }
                }
                painter.finish();
                if optimized { painter.recycle(&mut scratch); }
            }
            assert_eq!(actual, expected, "selection={selection:?}, clip={clip:?}");
        }
    }
}

#[test]
fn dense_layers_preserve_paint_budget_and_allow_updates_to_existing_cells() {
    let area = Rect::new(0, 0, 200, 200);
    let colors = EditorTheme::Dark.colors();
    let mut expected = Buffer::empty(area);
    let mut actual = expected.clone();
    for (buffer, optimized) in [(&mut expected, false), (&mut actual, true)] {
        let mut painter = Painter::new(buffer, area, colors, HashSet::new(), HashSet::new(), HashSet::new());
        let mut scratch = PaintScratch::default();
        if optimized { painter.take_scratch(&mut scratch); }
        for y in 0..area.height {
            for x in 0..area.width { painter.tint(x, y, 0.1); }
        }
        assert_eq!(painter.layers.len(), MAX_ANCHORS);
        painter.tint(0, 0, 0.4);
        painter.finish();
    }
    assert_eq!(actual, expected);
}

#[test]
fn adding_an_overlay_invalidates_cached_animation_exclusions() {
    let (_, mut geometry, _) = geometry_fixture();
    let before = geometry.protection(None, None, &[], &[]);
    assert_eq!(before.mask.as_ref().unwrap().flags((4, 3)), 0);
    geometry.overlay(3, 4, 2);
    let after = geometry.protection(None, None, &[], &[]);
    assert!(!Rc::ptr_eq(&before, &after));
    assert_eq!(after.mask.as_ref().unwrap().flags((4, 3)), animation_layers::EXCLUDED);
}

fn fill(
    buffer: &mut Buffer,
    colors: ThemeColors,
    scratch: &mut PaintScratch,
    reference: bool,
    amount: f32,
) {
    REFERENCE_PAINT.set(reference);
    let mut painter = Painter::new(
        buffer,
        Rect::new(0, 0, 100, 45),
        colors,
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    );
    if !reference {
        painter.take_scratch(scratch);
    }
    for y in 0..45 {
        for x in 0..100 {
            painter.tint_to(x, y, colors.fg_accent, amount);
        }
    }
    painter.finish();
    if !reference {
        painter.recycle(scratch);
    }
    REFERENCE_PAINT.set(false);
}

#[test]
fn paint_scratch_and_contrast_reuse_preserve_all_cells_across_theme_and_fill_changes() {
    let mut scratch = PaintScratch::default();
    for theme in [EditorTheme::Dark, EditorTheme::Light, EditorTheme::Dark] {
        let colors = theme.colors();
        for amount in [0.05, 0.2, 0.8, 0.1] {
            let mut expected = Buffer::empty(Rect::new(0, 0, 100, 45));
            for y in 0..45 {
                for x in 0..100 {
                    let fg = match x % 4 {
                        0 => Color::Rgb(125, 130, 135),
                        1 => Color::White,
                        2 => colors.fg_accent,
                        _ => Color::Reset,
                    };
                    expected[(x, y)]
                        .set_symbol("x")
                        .set_fg(fg)
                        .set_bg(if y % 2 == 0 {
                            colors.bg_editor
                        } else {
                            Color::Rgb(y as u8 * 3, 100, 180)
                        });
                }
            }
            let mut actual = expected.clone();
            fill(
                &mut expected,
                colors,
                &mut PaintScratch::default(),
                true,
                amount,
            );
            fill(&mut actual, colors, &mut scratch, false, amount);
            assert_eq!(actual, expected);
            assert!(
                scratch.layers.is_empty()
                    && scratch.scope_colors.is_empty()
                    && scratch.scope_shapes.is_empty()
                    && scratch.scope_endpoints.is_empty()
                    && scratch.contrast.is_empty()
            );
            assert!(scratch.layers.capacity() >= 4500);
            assert!(
                scratch.contrast.capacity() <= 448,
                "256-entry cache remains bounded"
            );
        }
    }
}

#[test]
fn repeated_color_runs_preserve_individual_symbols_and_mixed_effect_layers() {
    let area = Rect::new(2, 1, 60, 8);
    for theme in [EditorTheme::Dark, EditorTheme::Light] {
        let colors = theme.colors();
        let mut base = Buffer::empty(area);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                base[(x, y)].set_fg(if (x / 5) % 2 == 0 { colors.fg_text } else { Color::Reset })
                    .set_bg(if y % 2 == 0 { colors.bg_editor } else { Color::Rgb(70, 80, 90) });
            }
        }
        let mut expected = base.clone();
        let mut actual = base;
        for (buffer, reference) in [(&mut expected, true), (&mut actual, false)] {
            REFERENCE_PAINT.set(reference);
            let mut painter = Painter::new(buffer, area, colors,
                HashSet::new(), HashSet::new(), HashSet::new());
            let mut scratch = PaintScratch::default();
            painter.take_scratch(&mut scratch);
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    let amount = 0.05 + (x / 5) as f32 * 0.04;
                    if y % 3 != 0 { painter.tint_to(x, y, colors.fg_accent, amount); }
                    if y % 3 != 1 {
                        painter.symbol_to(x, y, if x % 2 == 0 { "┌" } else { "─" }, colors.fg_accent, amount);
                    }
                }
            }
            painter.finish();
            REFERENCE_PAINT.set(false);
        }
        assert_eq!(actual, expected);
        assert_eq!(actual[(2, 2)].symbol(), "┌");
        assert_eq!(actual[(3, 2)].symbol(), "─");
    }
}

#[test]
#[ignore = "manual release animation paint benchmark"]
fn benchmark_animation_responsiveness() {
    let colors = EditorTheme::Dark.colors();
    let (_, _, mut base) = geometry_fixture();
    base.set_style(
        base.area,
        Style::default()
            .fg(Color::Rgb(125, 130, 135))
            .bg(colors.bg_editor),
    );
    for reference in [true, false] {
        let mut scratch = PaintScratch::default();
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            for _ in 0..60 {
                let mut buffer = base.clone();
                fill(&mut buffer, colors, &mut scratch, reference, 0.4);
                black_box(buffer);
            }
            samples.push(start.elapsed() / 60);
        }
        samples.sort();
        eprintln!(
            "scope fill 4500 cells reference={reference}: {:?}",
            samples[3]
        );
    }
}

#[test]
fn recycling_geometry_retains_capacity_but_no_source_or_paint_state() {
    let (rope, mut geometry, buffer) = geometry_fixture();
    let capacity = geometry.anchors.capacity();
    geometry.set_wrapped(72);
    geometry.set_bounded_source();
    geometry.set_color_preview(true);
    geometry.protect(2, 2, 10);
    geometry.overlay(4, 3, 12);
    geometry.obstacle(6, 4, 14);
    geometry.map_scope_row(3, 0, 3);
    geometry.record_primary_cursor(2, 2, 1, Style::default());
    geometry.retain_validated(&buffer);
    geometry.source_offsets(&rope);
    geometry.protection(None, Some((1, 0, 20, 0)), &[], &[]);
    let old_projection = geometry.projection_id;
    let area = Rect::new(3, 5, 120, 50);
    geometry.reset(area, true);
    assert_eq!(geometry.anchors.capacity(), capacity);
    assert!(geometry.anchors.is_empty());
    assert_ne!(geometry.projection_id, old_projection);
    assert!(!geometry.retained);
    assert_eq!(geometry.area, area);
    assert_eq!(geometry.gradient_area, Some(area));
    assert!(geometry.wrap_width.is_none() && geometry.color_preview.is_none());
    assert!(!geometry.bounded_source);
    assert!(geometry.offsets.borrow().is_none() && geometry.protection.borrow().is_none());
    assert!(geometry.primary_cursor.is_none());
    assert!(geometry.scope_rows.is_empty() && geometry.omitted_lines.is_empty());
    assert!(geometry.protected.is_empty() && geometry.obstacles.is_empty() && geometry.overlays.is_empty());
    geometry.push(0, 0, 'z', 0, 0, 1);
    assert_eq!(geometry.at((0, 0)).map(|a| (a.x, a.y, a.ch)), Some((3, 5, 'z')));
}

#[test]
fn projection_reuse_requires_exact_source_geometry_and_keeps_current_protection() {
    let (rope, mut old, buffer) = geometry_fixture();
    old.retain_validated(&buffer);
    let offsets = old.source_offsets(&rope);
    old.source_cells();
    old.protection(None, Some((1, 0, 20, 0)), &[], &[]);
    let mut current = old.clone();
    current.projection_id = Geometry::next_projection();
    current.offsets.get_mut().take();
    current.source_cells.take();
    current.protection.get_mut().take();
    current.protected.insert((4, 4));
    current.overlays.insert((10, 10));
    current.primary_cursor = Some((Rect::new(8, 8, 1, 1), Style::default()));
    assert!(current.reuse_projection(&mut old));
    assert_eq!(current.projection_id, old.projection_id);
    assert!(Rc::ptr_eq(&current.source_offsets(&rope), &offsets));
    assert!(current.protection.borrow().is_none(), "previous selection/caret mask was reused");
    let mask = current.protection(None, None, &[], &[]);
    assert!(mask.protected_cells().contains(&(4, 4)));
    assert!(mask.caret_cells().contains(&(8, 8)));

    for change in 0..12 {
        let mut changed = current.clone();
        let mut previous = current.clone();
        changed.projection_id = Geometry::next_projection();
        match change {
            0 => changed.area.x += 1,
            1 => changed.anchors[0].ch = 'x',
            2 => changed.anchors[0].col += 1,
            3 => changed.scope_rows.push((5, 0, 5)),
            4 => changed.omitted_lines.push((5, 7, 5)),
            5 => { changed.unsafe_rows.insert(8); }
            6 => { changed.obstacles.insert((8, 8)); }
            7 => changed.wrap_width = Some(44),
            8 => changed.bounded_source = !changed.bounded_source,
            9 => changed.color_preview = Some(true),
            10 => changed.diff_projection = Some((5, true)),
            _ => changed.retained = false,
        }
        assert!(!changed.reuse_projection(&mut previous), "change={change}");
        assert_ne!(changed.projection_id, previous.projection_id);
    }
}

#[test]
fn repeated_ambiguous_scalars_only_retire_their_own_row() {
    let (_, mut geometry, _) = geometry_fixture();
    let expected: Vec<_> = geometry.anchors.iter().filter(|a| a.y != 40).copied().collect();
    for col in 0..4096 {
        geometry.push(40, col, '\u{301}', 40, 0, 0);
    }
    assert_eq!(geometry.anchors.len(), expected.len());
    for (a, b) in geometry.anchors.iter().zip(expected) {
        assert_eq!((a.line, a.col, a.x, a.y, a.ch), (b.line, b.col, b.x, b.y, b.ch));
    }
}

#[test]
#[ignore = "manual optimized pathological combining-row geometry comparison"]
fn benchmark_ambiguous_animation_row() {
    let mut times = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let (_, mut geometry, _) = geometry_fixture();
            let start = Instant::now();
            for col in 0..4096 {
                if optimized { geometry.push(40, col, '\u{301}', 40, 0, 0); }
                else {
                    geometry.unsafe_rows.insert(40);
                    geometry.invalidate_rows();
                    geometry.anchors.retain(|a| a.y != 40);
                }
            }
            times[usize::from(optimized)].push(start.elapsed());
            black_box(geometry);
        }
    }
    for samples in &mut times { samples.sort_unstable(); }
    eprintln!("4096 ambiguous animation scalars: before={:?}, after={:?}", times[0][3], times[1][3]);
}
