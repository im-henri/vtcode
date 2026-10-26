//! Reuse exact gradient colors across ordinary redraws and split editor panes.
use crate::app_theme::{GradientPrecomputed, ThemeGradientSpec, lerp_color};
use ratatui::{Frame, layout::Rect, style::Color};
use std::{cell::RefCell, collections::VecDeque};

const MAX_TILES: usize = 16;
const MAX_TILE_CELLS: usize = 65_536;
const MAX_CACHED_COLORS: usize = 262_144;

#[derive(PartialEq, Eq)]
struct Key {
    width: u16,
    height: u16,
    editor: (Color, Color, u32),
    line: (Color, Color, u32),
}

struct Tile {
    key: Key,
    editor: Option<Vec<Color>>,
    line: Option<Vec<Color>>,
}

impl Tile {
    fn colors(&self) -> usize {
        self.editor.as_ref().map_or(0, Vec::len) + self.line.as_ref().map_or(0, Vec::len)
    }
    fn new(key: Key) -> Self {
        let colors = |(start, end, angle): (Color, Color, u32)| {
            (start != end).then(|| {
                let projection =
                    GradientPrecomputed::new(key.width, key.height, f32::from_bits(angle));
                let mut colors =
                    Vec::with_capacity(usize::from(key.width) * usize::from(key.height));
                for y in 0..key.height {
                    for x in 0..key.width {
                        colors.push(lerp_color(start, end, projection.t_at(x, y)));
                    }
                }
                colors
            })
        };
        let editor = colors(key.editor);
        let line = colors(key.line);
        Self { key, editor, line }
    }
}

#[derive(Default)]
struct Cache {
    tiles: VecDeque<Tile>,
    colors: usize,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    editor_bg: Color,
    line_bg: Color,
    editor: ThemeGradientSpec,
    line: ThemeGradientSpec,
) {
    let cells = usize::from(area.width) * usize::from(area.height);
    if cells == 0 || editor.start == editor.end && line.start == line.end {
        return;
    }
    let key = Key {
        width: area.width,
        height: area.height,
        editor: (editor.start, editor.end, editor.angle_deg.to_bits()),
        line: (line.start, line.end, line.angle_deg.to_bits()),
    };
    let apply = |tile: &Tile, frame: &mut Frame| {
        let buffer = frame.buffer_mut();
        let visible = area.intersection(buffer.area);
        if visible.is_empty() {
            return;
        }
        for y in visible.y..visible.bottom() {
            let start = buffer.index_of(visible.x, y);
            let source =
                usize::from(y - area.y) * usize::from(area.width) + usize::from(visible.x - area.x);
            for (offset, cell) in buffer.content[start..start + usize::from(visible.width)]
                .iter_mut()
                .enumerate()
            {
                if cell.bg == editor_bg
                    && let Some(colors) = &tile.editor
                {
                    cell.set_bg(colors[source + offset]);
                } else if cell.bg == line_bg
                    && let Some(colors) = &tile.line
                {
                    cell.set_bg(colors[source + offset]);
                }
            }
        }
    };
    if cells > MAX_TILE_CELLS {
        // Never allocate a document-sized color table for synthetic/huge areas.
        let editor_projection = GradientPrecomputed::new(area.width, area.height, editor.angle_deg);
        let line_projection = GradientPrecomputed::new(area.width, area.height, line.angle_deg);
        let buffer = frame.buffer_mut();
        let visible = area.intersection(buffer.area);
        if visible.is_empty() {
            return;
        }
        for y in visible.y..visible.bottom() {
            for x in visible.x..visible.right() {
                let cell = &mut buffer[(x, y)];
                if cell.bg == editor_bg && editor.start != editor.end {
                    cell.set_bg(lerp_color(
                        editor.start,
                        editor.end,
                        editor_projection.t_at(x - area.x, y - area.y),
                    ));
                } else if cell.bg == line_bg && line.start != line.end {
                    cell.set_bg(lerp_color(
                        line.start,
                        line.end,
                        line_projection.t_at(x - area.x, y - area.y),
                    ));
                }
            }
        }
        return;
    }
    CACHE.with(|storage| {
        let mut cache = storage.borrow_mut();
        if let Some(index) = cache.tiles.iter().position(|tile| tile.key == key) {
            let tile = cache.tiles.remove(index).unwrap();
            cache.tiles.push_back(tile);
        } else {
            let tile = Tile::new(key);
            while !cache.tiles.is_empty()
                && (cache.tiles.len() >= MAX_TILES
                    || cache.colors + tile.colors() > MAX_CACHED_COLORS)
            {
                let old = cache.tiles.pop_front().unwrap();
                cache.colors -= old.colors();
            }
            cache.colors += tile.colors();
            cache.tiles.push_back(tile);
        }
        apply(cache.tiles.back().unwrap(), frame);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        Terminal,
        backend::TestBackend,
        style::{Modifier, Style},
    };

    fn render(area: Rect, angle: f32, shared_base: bool, cached: bool) -> ratatui::buffer::Buffer {
        let editor_bg = Color::Rgb(21, 31, 41);
        let line_bg = if shared_base {
            editor_bg
        } else {
            Color::Rgb(55, 65, 75)
        };
        let editor = ThemeGradientSpec {
            start: Color::Rgb(13, 23, 33),
            end: Color::Rgb(61, 71, 81),
            angle_deg: angle,
        };
        let line = ThemeGradientSpec {
            start: Color::Rgb(37, 47, 57),
            end: Color::Rgb(83, 93, 103),
            angle_deg: angle + 40.0,
        };
        let mut terminal = Terminal::new(TestBackend::new(84, 24)).unwrap();
        terminal
            .draw(|frame| {
                let buffer = frame.buffer_mut();
                for y in 0..24 {
                    for x in 0..84 {
                        let bg = match (x + y) % 3 {
                            0 => editor_bg,
                            1 => line_bg,
                            _ => Color::Rgb(90, 4, 200),
                        };
                        buffer[(x, y)].set_char('x').set_style(
                            Style::default()
                                .bg(bg)
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::ITALIC),
                        );
                    }
                }
                if cached {
                    draw(frame, area, editor_bg, line_bg, editor, line);
                } else {
                    crate::ui_editor_highlight::apply_editor_gradients_reference(
                        frame, area, editor_bg, line_bg, editor, line,
                    );
                }
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn cached_gradient_tiles_preserve_every_cell_across_angles_origins_and_overlays() {
        for angle in [0.0, 30.0, 90.0, 179.0, 270.0, -45.0] {
            for area in [
                Rect::new(0, 0, 80, 20),
                Rect::new(2, 1, 70, 15),
                Rect::new(70, 18, 30, 10),
            ] {
                for shared in [false, true] {
                    assert_eq!(
                        render(area, angle, shared, true),
                        render(area, angle, shared, false)
                    );
                }
            }
        }
    }

    #[test]
    fn oversized_or_outside_gradient_areas_do_not_allocate_unbounded_tiles() {
        let area = Rect::new(0, 0, 500, 500);
        let editor = ThemeGradientSpec {
            start: Color::Rgb(10, 20, 30),
            end: Color::Rgb(90, 100, 110),
            angle_deg: 31.0,
        };
        let mut terminal = Terminal::new(TestBackend::new(8, 6)).unwrap();
        terminal
            .draw(|frame| {
                frame
                    .buffer_mut()
                    .set_style(Rect::new(0, 0, 8, 6), Style::default().bg(editor.start));
                draw(frame, area, editor.start, editor.start, editor, editor);
                draw(
                    frame,
                    Rect::new(100, 100, 3, 3),
                    editor.start,
                    editor.start,
                    editor,
                    editor,
                );
            })
            .unwrap();
        let projection = GradientPrecomputed::new(area.width, area.height, editor.angle_deg);
        for y in 0..6 {
            for x in 0..8 {
                assert_eq!(
                    terminal.backend().buffer()[(x, y)].bg,
                    lerp_color(editor.start, editor.end, projection.t_at(x, y))
                );
            }
        }
        CACHE.with(|cache| {
            let cache = cache.borrow();
            assert!(cache.tiles.len() <= MAX_TILES && cache.colors <= MAX_CACHED_COLORS);
            assert!(cache.tiles.iter().all(|tile| usize::from(tile.key.width)
                * usize::from(tile.key.height)
                <= MAX_TILE_CELLS));
        });
    }

    #[test]
    #[ignore = "manual release editor gradient timing against exact transfer-function oracle"]
    fn benchmark_cached_editor_gradients() {
        use std::{hint::black_box, time::Instant};
        let mut colors = crate::app_theme::EditorTheme::Dark.colors();
        colors.gradients.bg_editor = ThemeGradientSpec {
            start: colors.bg_editor,
            end: Color::Rgb(53, 67, 83),
            angle_deg: 35.0,
        };
        colors.gradients.bg_line_highlight = ThemeGradientSpec {
            start: colors.bg_line_highlight,
            end: Color::Rgb(81, 93, 106),
            angle_deg: 70.0,
        };
        let area = Rect::new(0, 0, 180, 60);
        let mut results = Vec::new();
        for cached in [false, true] {
            let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
            let mut samples = Vec::new();
            for _ in 0..7 {
                let started = Instant::now();
                for _ in 0..100 {
                    terminal
                        .draw(|frame| {
                            frame
                                .buffer_mut()
                                .set_style(area, Style::default().bg(colors.bg_editor));
                            if cached {
                                draw(
                                    frame,
                                    area,
                                    colors.bg_editor,
                                    colors.bg_line_highlight,
                                    colors.gradients.bg_editor,
                                    colors.gradients.bg_line_highlight,
                                );
                            } else {
                                crate::ui_editor_highlight::apply_editor_gradients_reference(
                                    frame,
                                    area,
                                    colors.bg_editor,
                                    colors.bg_line_highlight,
                                    colors.gradients.bg_editor,
                                    colors.gradients.bg_line_highlight,
                                );
                            }
                        })
                        .unwrap();
                    black_box(terminal.backend().buffer());
                }
                samples.push(started.elapsed() / 100);
            }
            samples.sort();
            results.push(samples[3]);
        }
        eprintln!(
            "180x60 editor gradients direct={:?} retained exact colors={:?}",
            results[0], results[1]
        );
    }
}
