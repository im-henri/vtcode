//! A luminous, filled sweep confined to whitespace that was just inserted.
use super::{Anchor, Painter, cursor_blend, lerp_color};

#[path = "whitespace_motion.rs"]
mod motion;

pub(super) fn draw(
    painter: &mut Painter<'_>,
    inserted: &[(usize, Anchor)],
    progress: f32,
    strength: f32,
    paste: bool,
) {
    let trail = lerp_color(
        painter.colors.fg_accent_blue,
        painter.colors.fg_accent,
        0.20,
    );
    let leading = lerp_color(painter.colors.fg_accent, painter.colors.fg_text, 0.50);
    motion::sample_with(inserted, painter.area, progress, strength, paste, |cell| {
        // The theme supplies both the cool trailing ribbon and its hot edge.
        // Brightness is fitted to a fractional solid block, never dotted stars.
        let color = lerp_color(trail, leading, cell.heat);
        painter.tint_to(cell.x, cell.y, color, cell.halo * 1.35);
        // The compositor preserves visible whitespace markers, column rulers,
        // selected cells, and all carets; only a real blank receives the ribbon.
        let surface = painter
            .buffer
            .cell((cell.x, cell.y))
            .map_or(painter.colors.bg_editor, |source| source.bg);
        // Blend emitted light in linear space, as the cursor ribbon does. An
        // sRGB interpolation swallowed the cool tail on dark backgrounds and
        // made even a high-intensity leading edge look muddy.
        let ink = cursor_blend(surface, color, (cell.opacity * 1.65).clamp(0.0, 1.0));
        painter.symbol_to(cell.x, cell.y, cell.symbol, ink, 1.0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{buffer::Buffer, layout::Rect, style::Style};
    use std::collections::HashSet;

    fn anchors() -> Vec<(usize, Anchor)> {
        vec![
            (
                0,
                Anchor {
                    line: 0,
                    col: 0,
                    x: 2,
                    y: 1,
                    width: 8,
                    ch: '\t',
                },
            ),
            (
                1,
                Anchor {
                    line: 0,
                    col: 1,
                    x: 10,
                    y: 1,
                    width: 1,
                    ch: ' ',
                },
            ),
        ]
    }

    #[test]
    fn whitespace_ribbon_preserves_rulers_visible_markers_carets_selections_and_viewport() {
        let app = crate::app::App::test_new();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 14, 4));
        buffer.set_style(
            buffer.area,
            Style::default()
                .fg(app.colors.fg_text)
                .bg(app.colors.bg_editor),
        );
        buffer[(2, 1)].set_symbol("→");
        buffer[(3, 1)].set_symbol("│");
        buffer[(4, 1)].set_symbol("·");
        buffer[(11, 1)].set_symbol("x");
        buffer[(12, 1)].set_symbol("界");
        let before = buffer.clone();
        let mut painter = Painter::new(
            &mut buffer,
            Rect::new(2, 1, 9, 1),
            app.colors,
            HashSet::from([(5, 1), (8, 1)]),
            HashSet::from([(8, 1)]),
            HashSet::new(),
        );
        draw(&mut painter, &anchors(), 0.45, 1.0, false);
        painter.finish();
        for position in [(2, 1), (3, 1), (4, 1)] {
            assert_eq!(buffer[position].symbol(), before[position].symbol());
        }
        for position in [(5, 1), (8, 1), (11, 1), (12, 1), (1, 1), (3, 2)] {
            assert_eq!(buffer[position], before[position]);
        }
        assert_ne!(
            buffer, before,
            "the unprotected inserted whitespace is visibly animated"
        );
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| !matches!(cell.symbol(), "*" | "✦"))
        );
    }

    #[test]
    fn whitespace_ribbon_expiry_and_zero_intensity_leave_the_exact_original_frame() {
        let app = crate::app::App::test_new();
        for (progress, strength) in [(1.0, 1.0), (0.4, 0.0), (0.0, 1.0)] {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 14, 4));
            buffer.set_style(
                buffer.area,
                Style::default()
                    .fg(app.colors.fg_text)
                    .bg(app.colors.bg_editor),
            );
            let before = buffer.clone();
            let mut painter = Painter::new(
                &mut buffer,
                Rect::new(2, 1, 9, 1),
                app.colors,
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            draw(&mut painter, &anchors(), progress, strength, false);
            painter.finish();
            assert_eq!(buffer, before);
        }
    }
}
