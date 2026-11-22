//! The vendored ratatui-core changes per-cell frame paths for speed only.
//! Compare them with the published algorithms on varied buffers.
use ratatui::{
    buffer::{Buffer, Cell},
    layout::Rect,
    style::{Color, Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

/// ratatui-core 0.1.0 `Buffer::diff`, unchanged.
fn reference_diff<'a>(previous: &Buffer, next: &'a Buffer) -> Vec<(u16, u16, &'a Cell)> {
    let previous_buffer = &previous.content;
    let next_buffer = &next.content;
    let mut updates = vec![];
    let mut invalidated: usize = 0;
    let mut to_skip: usize = 0;
    for (i, (current, prev)) in next_buffer.iter().zip(previous_buffer.iter()).enumerate() {
        if !current.skip && (current != prev || invalidated > 0) && to_skip == 0 {
            let (x, y) = previous.pos_of(i);
            updates.push((x, y, &next_buffer[i]));
            let symbol = current.symbol();
            let cell_width = symbol.width();
            let contains_vs16 = symbol.chars().any(|c| c == '\u{FE0F}');
            if cell_width > 1 && contains_vs16 {
                for k in 1..cell_width {
                    let j = i + k;
                    if j >= next_buffer.len() || j >= previous_buffer.len() {
                        break;
                    }
                    let prev_trailing = &previous_buffer[j];
                    let next_trailing = &next_buffer[j];
                    if !next_trailing.skip && prev_trailing != next_trailing {
                        let (tx, ty) = previous.pos_of(j);
                        updates.push((tx, ty, next_trailing));
                    }
                }
            }
        }
        to_skip = current.symbol().width().saturating_sub(1);
        let affected_width = current.symbol().width().max(prev.symbol().width());
        invalidated = affected_width.max(invalidated).saturating_sub(1);
    }
    updates
}

fn reference_eq(a: &Cell, b: &Cell) -> bool {
    a.symbol() == b.symbol()
        && a.underline_color == b.underline_color
        && a.fg == b.fg
        && a.bg == b.bg
        && a.modifier == b.modifier
        && a.skip == b.skip
}

/// Small deterministic generator; the test needs variety, not randomness.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound
    }
}

const SYMBOLS: &[&str] = &[
    " ",
    "a",
    "~",
    "{",
    "\t",
    "\u{7f}",
    "é",
    "界",
    "🦀",
    "❤\u{FE0F}",
    "e\u{301}",
    "",
    "\u{200B}",
];
const COLORS: &[Color] = &[
    Color::Reset,
    Color::Red,
    Color::Rgb(10, 20, 30),
    Color::Indexed(42),
];

fn random_cell(rng: &mut Lcg) -> Cell {
    let mut cell = Cell::default();
    match rng.next(5) {
        0 => {}
        1 => {
            cell.set_symbol(" ");
        }
        _ => {
            cell.set_symbol(SYMBOLS[rng.next(SYMBOLS.len())]);
        }
    }
    cell.fg = COLORS[rng.next(COLORS.len())];
    cell.bg = COLORS[rng.next(COLORS.len())];
    cell.underline_color = COLORS[rng.next(COLORS.len())];
    if rng.next(4) == 0 {
        cell.modifier = Modifier::from_bits_truncate(rng.next(512) as u16);
    }
    cell.skip = rng.next(20) == 0;
    cell
}

fn random_buffer(rng: &mut Lcg, area: Rect, base: Option<&Buffer>) -> Buffer {
    let mut buffer = base.cloned().unwrap_or_else(|| Buffer::empty(area));
    let changes = if base.is_some() {
        rng.next(buffer.content.len() + 1)
    } else {
        buffer.content.len()
    };
    for _ in 0..changes {
        let index = rng.next(buffer.content.len());
        buffer.content[index] = random_cell(rng);
    }
    buffer
}

#[test]
fn patched_diff_matches_published_algorithm() {
    let mut rng = Lcg(7);
    for round in 0..400 {
        let area = Rect::new(
            rng.next(3) as u16,
            rng.next(3) as u16,
            1 + rng.next(24) as u16,
            1 + rng.next(6) as u16,
        );
        let previous = random_buffer(&mut rng, area, None);
        let next = random_buffer(&mut rng, area, (round % 2 == 0).then_some(&previous));
        let expected: Vec<_> = reference_diff(&previous, &next)
            .into_iter()
            .map(|(x, y, cell)| (x, y, cell.clone()))
            .collect();
        let actual: Vec<_> = previous
            .diff(&next)
            .into_iter()
            .map(|(x, y, cell)| (x, y, cell.clone()))
            .collect();
        assert_eq!(actual.len(), expected.len(), "round {round}");
        for (a, e) in actual.iter().zip(&expected) {
            assert!(
                a.0 == e.0 && a.1 == e.1 && reference_eq(&a.2, &e.2),
                "round {round}"
            );
        }
    }
}

#[test]
fn patched_cell_equality_matches_published_algorithm() {
    let mut rng = Lcg(11);
    for _ in 0..20_000 {
        let a = random_cell(&mut rng);
        let b = if rng.next(3) == 0 {
            a.clone()
        } else {
            random_cell(&mut rng)
        };
        assert_eq!(a == b, reference_eq(&a, &b));
    }
    let mut space = Cell::default();
    space.set_symbol(" ");
    assert_eq!(space, Cell::default());
}

#[test]
fn patched_set_style_matches_per_cell_styling() {
    let mut rng = Lcg(13);
    let styles = [
        Style::default().fg(Color::Red),
        Style::default()
            .bg(Color::Rgb(1, 2, 3))
            .add_modifier(Modifier::BOLD),
        Style::default()
            .underline_color(Color::Indexed(9))
            .remove_modifier(Modifier::BOLD),
    ];
    for _ in 0..300 {
        let area = Rect::new(
            rng.next(4) as u16,
            rng.next(4) as u16,
            rng.next(20) as u16,
            rng.next(8) as u16,
        );
        let target = Rect::new(
            rng.next(30) as u16,
            rng.next(12) as u16,
            rng.next(30) as u16,
            rng.next(12) as u16,
        );
        let style = styles[rng.next(styles.len())];
        let base = random_buffer(&mut rng, area, None);
        let mut expected = base.clone();
        let clipped = expected.area.intersection(target);
        for y in clipped.top()..clipped.bottom() {
            for x in clipped.left()..clipped.right() {
                expected[(x, y)].set_style(style);
            }
        }
        let mut actual = base;
        actual.set_style(target, style);
        assert_eq!(actual, expected, "area={area:?} target={target:?}");
    }
}
