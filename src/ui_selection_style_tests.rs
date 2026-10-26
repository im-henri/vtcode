use super::*;
use crate::app_theme::{EditorTheme, ThemeColors};
use ratatui::style::Modifier;
use std::{hint::black_box, time::Instant};

// Retain the eager background calculation and per-cell foreground lookup as
// an independent oracle and performance reference for this optimization.
struct Reference {
    colors: ThemeColors,
    background: Color,
    foregrounds: rustc_hash::FxHashMap<Color, Color>,
}

impl Reference {
    fn new(colors: ThemeColors) -> Self {
        let preferred = colors.bg_selection;
        let visible = |candidate| {
            [colors.bg_editor, colors.bg_line_highlight]
                .into_iter()
                .all(|surface| contrast_ratio(candidate, surface).is_none_or(|ratio| ratio >= 1.6))
        };
        let background = if colors.ui.preserve_theme_colors || visible(preferred) {
            preferred
        } else {
            colors
                .ui
                .contrast_palette
                .into_iter()
                .filter_map(|target| {
                    (1..=32)
                        .map(|step| {
                            crate::app_theme::lerp_color(preferred, target, step as f32 / 32.0)
                        })
                        .find(|&candidate| visible(candidate))
                })
                .min_by_key(|&candidate| match (preferred, candidate) {
                    (Color::Rgb(r, g, b), Color::Rgb(cr, cg, cb)) => {
                        (i32::from(r) - i32::from(cr)).pow(2)
                            + (i32::from(g) - i32::from(cg)).pow(2)
                            + (i32::from(b) - i32::from(cb)).pow(2)
                    }
                    _ => 0,
                })
                .unwrap_or(preferred)
        };
        Self {
            colors,
            background,
            foregrounds: Default::default(),
        }
    }

    fn apply(&mut self, style: Style) -> Style {
        let foreground = self
            .colors
            .ui
            .fg_editor_selection
            .or(style.fg)
            .unwrap_or(self.colors.fg_text);
        let foreground = *self.foregrounds.entry(foreground).or_insert_with(|| {
            ensure_contrast(
                foreground,
                self.background,
                4.5,
                contrast_colors(&self.colors.ui.contrast_fallbacks()),
            )
        });
        style.bg(self.background).fg(foreground)
    }
}

#[test]
fn lazy_selection_styles_match_eager_resolution_across_native_and_imported_palettes() {
    let mut colors = EditorTheme::Dark.colors();
    for preserve in [false, true] {
        colors.ui.preserve_theme_colors = preserve;
        for background in [
            Color::Rgb(20, 20, 20),
            Color::Rgb(200, 200, 200),
            Color::Indexed(3),
            Color::Reset,
        ] {
            colors.bg_selection = background;
            colors.bg_editor = background;
            colors.bg_line_highlight = Color::Rgb(60, 55, 50);
            for foreground in [None, Some(Color::Rgb(1, 2, 3)), Some(Color::Reset)] {
                colors.ui.fg_editor_selection = foreground;
                let mut reference = Reference::new(colors);
                let mut actual = EditorSelectionStyle::new(&colors);
                assert_eq!(
                    actual.background, background,
                    "construction must not resolve unused selection colors"
                );
                for pass in 0..3 {
                    for token in [
                        None,
                        Some(Color::Red),
                        Some(Color::Rgb(16, 20, 30)),
                        Some(Color::Rgb(245, 240, 235)),
                    ] {
                        let mut style = Style::default()
                            .bg(Color::Blue)
                            .underline_color(Color::Yellow)
                            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                            .remove_modifier(if pass == 0 {
                                Modifier::ITALIC
                            } else {
                                Modifier::DIM
                            });
                        style.fg = token;
                        for _ in 0..8 {
                            assert_eq!(actual.apply(style), reference.apply(style));
                        }
                    }
                }
                assert!(actual.pending_surfaces.is_none());
                if preserve {
                    assert!(
                        actual.foregrounds.is_empty(),
                        "preserved theme colors need no hash cache"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "manual eager/lazy selection preparation and painting comparison"]
fn benchmark_selection_style_work() {
    for (case, preserve, cells) in [
        ("unused native selection", false, 0),
        ("native selection", false, 20_000),
        ("imported selection", true, 20_000),
    ] {
        let mut colors = EditorTheme::Dark.colors();
        colors.bg_selection = colors.bg_editor;
        colors.ui.preserve_theme_colors = preserve;
        let styles: Vec<_> = (0..20_000)
            .map(|i| {
                Style::default()
                    .fg([Color::Red, Color::Green, Color::Yellow, Color::Cyan][(i / 16) % 4])
            })
            .collect();
        let mut times = [Vec::new(), Vec::new()];
        for sample in 0..9 {
            for index in [sample % 2, (sample + 1) % 2] {
                let started = Instant::now();
                let repeats = if cells == 0 { 1000 } else { 20 };
                for _ in 0..repeats {
                    if index == 0 {
                        let mut reference = Reference::new(*black_box(&colors));
                        for &style in &styles[..cells] {
                            black_box(reference.apply(style));
                        }
                        black_box(reference.background);
                    } else {
                        let mut current = EditorSelectionStyle::new(black_box(&colors));
                        for &style in &styles[..cells] {
                            black_box(current.apply(style));
                        }
                        black_box(current.background);
                    }
                }
                times[index].push(started.elapsed() / repeats);
            }
        }
        for times in &mut times {
            times.sort_unstable();
        }
        eprintln!(
            "{case}: reference={:?}, current={:?}",
            times[0][4], times[1][4]
        );
    }
}
