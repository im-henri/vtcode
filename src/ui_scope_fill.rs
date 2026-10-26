//! A second, finite phase fills the inside of the completed scope contour.
use super::*;
use ratatui::style::Color;
use std::collections::HashSet;
use std::rc::Rc;

pub(in crate::ui_editor_animations) struct Ownership {
    source_rows: HashSet<u16>,
    limits: BTreeMap<u16, (u16, u16)>,
}

fn ownership(geometry: &Geometry, pair: Pair) -> Rc<Ownership> {
    if geometry.is_retained()
        && let Some((_, rows)) = geometry.fill_rows.borrow().iter().find(|(key, _)| *key == pair)
    {
        return Rc::clone(rows);
    }
    let mut source_rows = HashSet::new();
    let mut limits = BTreeMap::<u16, (u16, u16)>::new();
    for &(line, _, y) in &geometry.scope_rows {
        if line >= pair.0.0 && line <= pair.1.0 { source_rows.insert(y); }
    }
    for anchor in &geometry.anchors {
        if bubble::contains(pair, anchor.line, anchor.col) {
            source_rows.insert(anchor.y);
        } else {
            let limit = limits.entry(anchor.y).or_insert((geometry.area.x, geometry.area.right()));
            if (anchor.line, anchor.col) < pair.0 {
                limit.0 = limit.0.max(anchor.x.saturating_add(anchor.width));
            } else {
                limit.1 = limit.1.min(anchor.x);
            }
        }
    }
    let rows = Rc::new(Ownership { source_rows, limits });
    if geometry.is_retained() {
        let mut cached = geometry.fill_rows.borrow_mut();
        if cached.len() >= 6 { cached.remove(0); }
        cached.push((pair, Rc::clone(&rows)));
    }
    rows
}

/// Unclipped inner edges keep the reveal stable when scrolling horizontally.
pub(super) struct Span {
    pub(super) y: u16,
    pub(super) left: i64,
    pub(super) right: i64,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    pair: Pair,
    spans: &[Span],
    color: Color,
    progress: f32,
    energy: f32,
) -> bool {
    if progress <= 0.0 || energy <= 0.002 {
        return false;
    }
    // Ownership belongs to this geometry, not the route: inline neighbors and
    // diff-only rows can change independently of the contour. A retained scene
    // can reuse it, while edits, scrolling and occlusion invalidate it.
    let ownership = ownership(geometry, pair);
    let front = smooth(progress) * 1.3;
    let attack = smooth(progress / 0.2);
    let mut visible = false;
    let mut budget = super::super::MAX_ANCHORS;
    for span in spans {
        if !ownership.source_rows.contains(&span.y) || span.right <= span.left + 1 {
            continue;
        }
        let (left, right) = ownership.limits
            .get(&span.y)
            .copied()
            .unwrap_or((geometry.area.x, geometry.area.right()));
        let start = (span.left + 1)
            .max(i64::from(left))
            .max(i64::from(geometry.area.x));
        let end = span
            .right
            .min(i64::from(right))
            .min(i64::from(geometry.area.right()));
        let half_width = (span.right - span.left - 1) as f32 * 0.5;
        for x in start..end {
            if budget == 0 {
                return visible;
            }
            budget -= 1;
            let position = (x as u16, span.y);
            if painter.is_caret(position)
                || (painter.mask.is_none() && geometry.obstacles.contains(&position)) {
                continue;
            }
            let depth = ((x - span.left).min(span.right - x) as f32 - 0.5) / half_width;
            let reveal = smooth((front - depth) / 0.3) * attack;
            let amount = reveal * energy * 0.16;
            visible |= painter.tint_to(position.0, position.1, color, amount);
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use crate::{app_theme::EditorTheme, ui_editor_animations::REFERENCE_RETAINED_GEOMETRY};

    fn fixture() -> (Geometry, Buffer) {
        let mut geometry = Geometry::new(Rect::new(2, 1, 100, 45), true);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 106, 48));
        for row in 0..45 {
            for (col, ch) in "    calculate(value, other);".chars().enumerate() {
                geometry.push(row, col, ch, row, col, 1);
                buffer[(2 + col as u16, 1 + row as u16)].set_char(ch);
            }
            geometry.map_scope_row(row, 0, row);
        }
        geometry.obstacle(5, 60, 3);
        geometry.overlay(7, 65, 4);
        geometry.retain_validated(&buffer);
        (geometry, buffer)
    }

    #[test]
    fn fill_ownership_is_bounded_and_invalidates_with_geometry() {
        let (mut geometry, buffer) = fixture();
        let pair = ((1, 5), (12, 15));
        let initial = ownership(&geometry, pair);
        assert!(Rc::ptr_eq(&initial, &ownership(&geometry, pair)));
        geometry.occlude_top(3);
        geometry.retain_validated(&buffer);
        let updated = ownership(&geometry, pair);
        assert!(!Rc::ptr_eq(&initial, &updated));
        assert!(!updated.source_rows.contains(&1));
        for line in 0..50 { ownership(&geometry, ((line, 0), (line + 2, 5))); }
        assert!(geometry.fill_rows.borrow().len() <= 6);
    }

    #[test]
    fn cached_fill_and_dense_masks_match_fresh_ownership_for_partial_scopes_and_selections() {
        let (geometry, base) = fixture();
        let spans: Vec<_> = (1..46).map(|y| Span { y, left: 2, right: 100 }).collect();
        for pair in [((1, 5), (12, 15)), ((5, 7), (5, 20)), ((0, 0), (44, 30))] {
            for selection in [None, Some((3, 1, 10, 12))] {
                let protection = geometry.protection(None, selection, &[], &[]);
                for progress in [0.05, 0.4, 0.8, 1.0] {
                    for theme in [EditorTheme::Dark, EditorTheme::Light] {
                        let colors = theme.colors();
                        let mut expected = base.clone();
                        let mut actual = base.clone();
                        for (buffer, cached) in [(&mut expected, false), (&mut actual, true)] {
                            REFERENCE_RETAINED_GEOMETRY.set(!cached);
                            let mut painter = Painter::new(buffer, geometry.area, colors,
                                protection.protected_cells(), protection.caret_cells(), geometry.unsafe_rows.clone());
                            painter.overlays = std::borrow::Cow::Borrowed(&geometry.overlays);
                            if cached { painter.mask = protection.mask.as_ref(); }
                            draw(&mut painter, &geometry, pair, &spans, colors.fg_accent, progress, 0.7);
                            painter.finish();
                            REFERENCE_RETAINED_GEOMETRY.set(false);
                        }
                        assert_eq!(actual, expected, "pair={pair:?} selection={selection:?} progress={progress}");
                    }
                }
            }
        }
    }
}
