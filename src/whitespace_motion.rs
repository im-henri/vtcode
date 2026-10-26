//! A finite, fractional light ribbon measured in displayed whitespace cells.
//!
//! Source offsets select the inserted range before this module is called. Tabs
//! contribute their actual renderer width, and line breaks are disconnected
//! spatially while sharing the same clock. Sampling never advances that clock.
use crate::animations::Anchor;
use ratatui::layout::Rect;

const MAX_ANCHORS: usize = 256;
const MAX_CELLS: usize = 1_024;
const COLUMNS: usize = 8;
const ROWS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RibbonCell {
    pub x: u16,
    pub y: u16,
    pub symbol: &'static str,
    /// Fitted brightness inside the selected solid block, rather than its area.
    pub opacity: f32,
    pub halo: f32,
    pub heat: f32,
}

#[derive(Clone, Copy)]
struct Span {
    anchor: Anchor,
    distance: f32,
    width: usize,
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn travel(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (10.0 + t * (-15.0 + t * 6.0))
}

fn arrival_time(distance: f32, total: f32, sweep: f32) -> f32 {
    // This inverse is evaluated once per tab, never for every subcell sample.
    let target = (distance / (total + 0.6)).clamp(0.0, 1.0);
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..12 {
        let mid = (low + high) * 0.5;
        if travel(mid) < target {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) * 0.5 * sweep
}

const fn rectangle(left: usize, top: usize, right: usize, bottom: usize) -> u64 {
    let mut mask = 0;
    let mut row = top;
    while row < bottom {
        let mut column = left;
        while column < right {
            mask |= 1u64 << (row * COLUMNS + column);
            column += 1;
        }
        row += 1;
    }
    mask
}

// A thin lower ribbon has useful solid Unicode geometry. Quadrants and edge
// strips let its leading edge move fractionally instead of stepping dots.
const GLYPHS: [(&str, u64); 17] = [
    ("▁", rectangle(0, 7, 8, 8)),
    ("▂", rectangle(0, 6, 8, 8)),
    ("▃", rectangle(0, 5, 8, 8)),
    ("▄", rectangle(0, 4, 8, 8)),
    ("▅", rectangle(0, 3, 8, 8)),
    ("▆", rectangle(0, 2, 8, 8)),
    ("▇", rectangle(0, 1, 8, 8)),
    ("▖", rectangle(0, 4, 4, 8)),
    ("▗", rectangle(4, 4, 8, 8)),
    ("▏", rectangle(0, 0, 1, 8)),
    ("▎", rectangle(0, 0, 2, 8)),
    ("▍", rectangle(0, 0, 3, 8)),
    ("▌", rectangle(0, 0, 4, 8)),
    ("▕", rectangle(7, 0, 8, 8)),
    ("▐", rectangle(4, 0, 8, 8)),
    ("▟", rectangle(0, 4, 8, 8) | rectangle(4, 0, 8, 4)),
    ("█", u64::MAX),
];

fn fit(field: &[f32; COLUMNS * ROWS]) -> (&'static str, f32) {
    let mut best = ("▁", 0.0);
    let mut best_score = 0.0;
    for &(symbol, mask) in &GLYPHS {
        let mut sum = 0.0;
        let mut bits = mask;
        while bits != 0 {
            let index = bits.trailing_zeros() as usize;
            sum += field[index];
            bits &= bits - 1;
        }
        let average = sum / mask.count_ones() as f32;
        // Least-squares fitting jointly chooses the shape and its brightness.
        // A partial leading cell transfers light continuously as it fills.
        let score = sum * average;
        if score > best_score {
            best = (symbol, average);
            best_score = score;
        }
    }
    best
}

#[cfg(test)]
pub(super) fn sample(
    inserted: &[(usize, Anchor)],
    area: Rect,
    progress: f32,
    strength: f32,
    paste: bool,
) -> Vec<RibbonCell> {
    let mut cells = Vec::new();
    sample_with(inserted, area, progress, strength, paste, |cell| {
        cells.push(cell)
    });
    cells
}

pub(super) fn sample_with(
    inserted: &[(usize, Anchor)],
    area: Rect,
    progress: f32,
    strength: f32,
    paste: bool,
    mut emit: impl FnMut(RibbonCell),
) {
    if !progress.is_finite()
        || !strength.is_finite()
        || progress <= 0.0
        || progress >= 1.0
        || strength <= 0.0
        || area.is_empty()
    {
        return;
    }
    let strength = strength.clamp(0.0, 1.0);
    let mut span_count = 0;
    let mut length = 0;
    for &(_, anchor) in inserted {
        if !matches!(anchor.ch, ' ' | '\t') || anchor.width == 0 {
            continue;
        }
        if span_count == MAX_ANCHORS || length == MAX_CELLS {
            break;
        }
        let width = (anchor.width as usize).min(MAX_CELLS - length);
        span_count += 1;
        length += width;
    }
    if length == 0 {
        return;
    }

    let total = length as f32;
    let sweep = if paste { 0.76 } else { 0.64 };
    let front = (total + 0.6) * travel(progress / sweep);
    let envelope = smooth(progress / 0.055) * (1.0 - smooth((progress - 0.52) / 0.48)) * strength;
    let tail = (total.sqrt() * 0.7).clamp(1.25, 5.0);
    let mut distance = 0;
    for &(_, anchor) in inserted
        .iter()
        .filter(|(_, a)| matches!(a.ch, ' ' | '\t') && a.width != 0)
        .take(span_count)
    {
        let width = (anchor.width as usize).min(MAX_CELLS - distance);
        let span = Span {
            anchor,
            distance: distance as f32,
            width,
        };
        distance += width;
        if span.anchor.y < area.y || span.anchor.y >= area.bottom() {
            continue;
        }
        let arrival = if span.anchor.ch == '\t' {
            let age =
                progress - arrival_time(span.distance + span.width as f32 - 0.1, total, sweep);
            smooth(age / 0.055) * (1.0 - smooth((age - 0.055) / 0.25))
        } else {
            0.0
        };
        for dx in 0..span.width {
            let x = u32::from(span.anchor.x) + dx as u32;
            if x < u32::from(area.x) || x >= u32::from(area.right()) || x > u16::MAX as u32 {
                continue;
            }
            let last_tab_cell = span.anchor.ch == '\t' && dx + 1 == span.width;
            let mut field = [0.0; COLUMNS * ROWS];
            let mut energy = 0.0;
            let mut thermal = 0.0;
            for column in 0..COLUMNS {
                let fraction = (column as f32 + 0.5) / COLUMNS as f32;
                let position = span.distance + dx as f32 + fraction;
                let lag = front - position;
                let ahead_edge = smooth((lag + 0.28) / 0.56);
                let head = (-((lag - 0.18) / 0.68).powi(2)).exp() * ahead_edge;
                let tail_light = (-lag.max(0.0) / tail).exp() * ahead_edge;
                let height = 0.11 + 0.29 * head;
                let light = (0.30 + 0.70 * head) * tail_light;
                for row in 0..ROWS {
                    let top = row as f32 / ROWS as f32;
                    let bottom = top + 1.0 / ROWS as f32;
                    // Integrating the moving horizontal edge within a sample
                    // keeps its energy continuous even below the grid spacing.
                    let vertical =
                        ((bottom - (1.0 - height)).clamp(0.0, 1.0 / ROWS as f32)) * ROWS as f32;
                    let mut value = light * vertical;
                    if last_tab_cell {
                        let tip = smooth((fraction - 0.72) / 0.20) * smooth((bottom - 0.20) / 0.20);
                        value = value.max(tip * arrival * 0.65);
                    }
                    let index = row * COLUMNS + column;
                    field[index] = value;
                    energy += value;
                    thermal += value * head.max(if last_tab_cell { arrival } else { 0.0 });
                }
            }
            if energy <= 0.000_01 {
                continue;
            }
            let (symbol, brightness) = fit(&field);
            let opacity = brightness * envelope * 0.94;
            if opacity <= 0.001 {
                continue;
            }
            emit(RibbonCell {
                x: x as u16,
                y: span.anchor.y,
                symbol,
                opacity,
                halo: energy / (COLUMNS * ROWS) as f32 * envelope * 0.34,
                heat: (thermal / energy).clamp(0.0, 1.0),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(x: u16, y: u16, ch: char, width: u16) -> (usize, Anchor) {
        (
            x as usize,
            Anchor {
                line: y as usize,
                col: x as usize,
                x,
                y,
                ch,
                width,
            },
        )
    }

    fn energy(cells: &[RibbonCell]) -> f32 {
        cells.iter().map(|cell| cell.halo).sum()
    }

    #[test]
    fn whitespace_display_width_drives_the_sweep_instead_of_source_character_count() {
        let area = Rect::new(0, 0, 20, 3);
        let tab = [anchor(2, 1, '\t', 6)];
        let spaces: Vec<_> = (2..8).map(|x| anchor(x, 1, ' ', 1)).collect();
        // Before tab arrival, six displayed spaces and a six-cell tab have
        // exactly the same motion, shape, and light distribution.
        assert_eq!(
            sample(&tab, area, 0.25, 0.8, false),
            sample(&spaces, area, 0.25, 0.8, false)
        );
        let short = [anchor(2, 1, '\t', 2)];
        let long_cells = sample(&tab, area, 0.30, 0.8, false);
        let short_cells = sample(&short, area, 0.30, 0.8, false);
        assert!(long_cells.iter().any(|cell| cell.x > 3));
        assert!(short_cells.iter().all(|cell| cell.x <= 3));
    }

    #[test]
    fn whitespace_fractional_motion_changes_light_between_terminal_columns() {
        let area = Rect::new(0, 0, 20, 3);
        let inserted = [anchor(2, 1, '\t', 8)];
        let mut previous = sample(&inserted, area, 0.18, 1.0, false);
        let mut changed = 0;
        for step in 1..=100 {
            let frame = sample(&inserted, area, 0.18 + step as f32 * 0.000_1, 1.0, false);
            assert!((energy(&frame) - energy(&previous)).abs() < 0.005);
            if frame != previous {
                changed += 1;
            }
            previous = frame;
        }
        assert_eq!(changed, 100, "motion must continue inside a cell");
        assert!(
            previous
                .iter()
                .all(|cell| !matches!(cell.symbol, "·" | "→"))
        );
    }

    #[test]
    fn whitespace_strength_changes_brightness_without_changing_motion_or_shape() {
        let area = Rect::new(0, 0, 20, 3);
        let inserted = [anchor(2, 1, '\t', 6)];
        let strong = sample(&inserted, area, 0.35, 1.0, false);
        let soft = sample(&inserted, area, 0.35, 0.5, false);
        assert_eq!(strong.len(), soft.len());
        for (strong, soft) in strong.iter().zip(&soft) {
            assert_eq!(
                (strong.x, strong.y, strong.symbol),
                (soft.x, soft.y, soft.symbol)
            );
            assert!((strong.opacity * 0.5 - soft.opacity).abs() < 0.000_01);
            assert!((strong.halo * 0.5 - soft.halo).abs() < 0.000_01);
        }
    }

    #[test]
    fn whitespace_tabs_land_at_their_actual_stops_and_keep_each_line_disconnected() {
        let area = Rect::new(0, 0, 20, 5);
        let inserted = [anchor(2, 1, '\t', 3), anchor(9, 3, '\t', 5)];
        let sample_at = sample(&inserted, area, 0.60, 1.0, false);
        assert!(sample_at.iter().any(|cell| cell.x == 13 && cell.y == 3));
        assert!(sample_at.iter().all(|cell| {
            (cell.y == 1 && (2..5).contains(&cell.x)) || (cell.y == 3 && (9..14).contains(&cell.x))
        }));
        assert!(sample_at.iter().any(|cell| cell.x == 13 && cell.heat > 0.5));
    }

    #[test]
    fn whitespace_clipping_does_not_retime_visible_motion_and_large_pastes_are_bounded() {
        let inserted: Vec<_> = (0..1_000).map(|x| anchor(x, 1, '\t', 8)).collect();
        let full_area = Rect::new(0, 0, 2_000, 3);
        let clipped_area = Rect::new(2, 1, 7, 1);
        let full = sample(&inserted, full_area, 0.40, 0.8, true);
        let clipped = sample(&inserted, clipped_area, 0.40, 0.8, true);
        assert!(full.len() <= MAX_CELLS);
        let expected: Vec<_> = full
            .into_iter()
            .filter(|cell| clipped_area.contains((cell.x, cell.y).into()))
            .collect();
        assert_eq!(clipped, expected);
    }

    #[test]
    fn whitespace_starts_without_a_flash_and_fades_completely_at_expiry() {
        let area = Rect::new(0, 0, 20, 3);
        let inserted = [anchor(2, 1, '\t', 6)];
        assert!(sample(&inserted, area, 0.0, 1.0, false).is_empty());
        assert!(sample(&inserted, area, 1.0, 1.0, false).is_empty());
        assert!(sample(&inserted, area, 0.5, 0.0, false).is_empty());
        assert!(sample(&inserted, area, f32::NAN, 1.0, false).is_empty());
        let before = energy(&sample(&inserted, area, 0.80, 1.0, false));
        let late = energy(&sample(&inserted, area, 0.95, 1.0, false));
        assert!(before > late && late < 0.01);
        let still = sample(&inserted, area, 0.42, 1.0, false);
        assert_eq!(still, sample(&inserted, area, 0.42, 1.0, false));
    }
}
