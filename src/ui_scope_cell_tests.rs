use super::*;
use crate::app_theme::EditorTheme;
use ratatui::{buffer::Buffer, style::Style};
use std::{borrow::Cow, hint::black_box};

#[test]
fn dense_scope_masks_preserve_overlays_unsafe_rows_and_wide_glyphs() {
    let mut geometry = Geometry::new(Rect::new(4, 2, 24, 12), true);
    geometry.protected.extend([(3, 3), (5, 3), (8, 7)]);
    geometry.overlays.extend([(2, 4), (8, 4), (10, 8)]);
    geometry.unsafe_rows.extend([1, 8]);
    geometry.primary_cursor = Some((Rect::new(7, 5, 2, 1), Style::default()));
    let protection = geometry.protection(None, None, &[], &[]);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 32, 18));
    for (x, y, symbol) in [
        (10, 3, "界"),
        (12, 4, "🌍"),
        (15, 6, "x"),
        (20, 7, "→"),
        (4, 9, "·"),
    ] {
        buffer[(x, y)].set_symbol(symbol);
    }
    buffer[(19, 10)].skip = true;
    let mut painter = Painter::new(
        &mut buffer,
        geometry.area,
        EditorTheme::Dark.colors(),
        protection.protected_cells(),
        protection.caret_cells(),
        geometry.unsafe_rows.clone(),
    );
    painter.overlays = Cow::Borrowed(&geometry.overlays);
    let positions: Vec<_> = (0..20)
        .flat_map(|y| (0..35).map(move |x| (x, y)))
        .chain([(u16::MAX, 2), (4, u16::MAX)])
        .collect();
    let expected: Vec<_> = positions
        .iter()
        .map(|&(x, y)| (route_cell(&painter, x, y), free_cell(&painter, x, y)))
        .collect();
    painter.mask = protection.mask.as_ref();
    for (&(x, y), &expected) in positions.iter().zip(&expected) {
        assert_eq!(
            (route_cell(&painter, x, y), free_cell(&painter, x, y)),
            expected,
            "cell ({x}, {y})"
        );
    }
    assert!(
        route_cell(&painter, 8, 4),
        "annotations permit route planning"
    );
    assert!(
        !free_cell(&painter, 8, 4),
        "annotations must never be painted over"
    );
    assert!(
        !route_cell(&painter, 10, 8),
        "unsafe rows override annotations"
    );
    assert!(
        !free_cell(&painter, 11, 3),
        "the continuation of a wide glyph is not free"
    );
}

#[test]
#[ignore = "comparative release benchmark"]
fn benchmark_editor_scope_cell_checks() {
    let mut geometry = Geometry::new(Rect::new(4, 1, 172, 58), true);
    for row in 0..58 {
        geometry.protect(row, 5, 30);
        geometry.overlay(row, 70, 30);
    }
    geometry.unsafe_rows.extend([7, 19]);
    let protection = geometry.protection(None, None, &[], &[]);
    let mut buffer = Buffer::empty(Rect::new(0, 0, 180, 60));
    let mut painter = Painter::new(
        &mut buffer,
        geometry.area,
        EditorTheme::Dark.colors(),
        protection.protected_cells(),
        protection.caret_cells(),
        geometry.unsafe_rows.clone(),
    );
    painter.overlays = Cow::Borrowed(&geometry.overlays);
    let mut old = Vec::new();
    let mut new = Vec::new();
    for round in 0..9 {
        for optimized in [round % 2 == 0, round % 2 != 0] {
            painter.mask = if optimized {
                protection.mask.as_ref()
            } else {
                None
            };
            let start = Instant::now();
            for _ in 0..100 {
                for y in 1..59 {
                    for x in 4..176 {
                        black_box(free_cell(black_box(&painter), x, y));
                    }
                }
            }
            let elapsed = start.elapsed() / 100;
            if optimized {
                new.push(elapsed);
            } else {
                old.push(elapsed);
            }
        }
    }
    old.sort_unstable();
    new.sort_unstable();
    eprintln!(
        "scope cell checks: sparse {:?}, dense {:?}, {:.2}x",
        old[4],
        new[4],
        old[4].as_secs_f64() / new[4].as_secs_f64()
    );
}
