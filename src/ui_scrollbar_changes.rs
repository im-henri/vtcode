//! Scrollbar change coverage uses solid half-cell blocks so even a single
//! changed line stays visible in a long file. Projection is bounded by the
//! scrollbar height.
const SUBROWS: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChangeRow([u8; SUBROWS]);

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ChangeRows(Vec<ChangeRow>);

fn priority(status: char) -> u8 {
    match status {
        'N' => 4,
        'U' | 'A' | 'M' => 3,
        'D' => 2,
        'S' => 1,
        _ => 0,
    }
}

impl ChangeRow {
    pub(crate) fn status(self) -> Option<char> {
        match self.0.into_iter().max().unwrap_or(0) {
            4 => Some('N'),
            3 => Some('U'),
            2 => Some('D'),
            1 => Some('S'),
            _ => None,
        }
    }

    pub(crate) fn symbol(self) -> char {
        let [upper, lower] = self.0;
        match (upper != 0, lower != 0) {
            (false, false) => ' ',
            (true, false) => '▀',
            (false, true) => '▄',
            (true, true) => '█',
        }
    }
}

impl ChangeRows {
    pub(crate) fn row(&self, index: usize) -> ChangeRow {
        self.0.get(index).copied().unwrap_or_default()
    }

    /// Query source-line intervals intersecting each half cell. Treating a
    /// changed line as an interval, rather than a point, also fills proportional
    /// spans correctly when the track has more samples than source lines.
    pub(crate) fn project(
        height: usize,
        total: usize,
        mut query: impl FnMut(usize, usize) -> Option<char>,
    ) -> Self {
        let mut rows = vec![ChangeRow::default(); height];
        let samples = height * SUBROWS;
        let total = total.max(1);
        for (row, coverage) in rows.iter_mut().enumerate() {
            for (part, slot) in coverage.0.iter_mut().enumerate() {
                let index = row * SUBROWS + part;
                let start = (index as u128 * total as u128 / samples as u128) as usize;
                let end =
                    (((index + 1) as u128 * total as u128).div_ceil(samples as u128)) as usize;
                *slot = query(start, end).map_or(0, priority);
            }
        }
        Self(rows)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.iter().all(|row| row.status().is_none())
    }

    /// Independent forward projection for reference tests. Production uses
    /// indexed range queries above and is bounded by scrollbar height.
    #[cfg(test)]
    pub(crate) fn from_lines(
        height: usize,
        total: usize,
        lines: impl IntoIterator<Item = (usize, char)>,
    ) -> Self {
        let mut rows = vec![ChangeRow::default(); height];
        let samples = height * SUBROWS;
        let total = total.max(1);
        for (line, status) in lines {
            let line = line.min(total - 1);
            let start = (line as u128 * samples as u128 / total as u128) as usize;
            let end = (((line + 1) as u128 * samples as u128).div_ceil(total as u128)) as usize;
            for index in start..end {
                let slot = &mut rows[index / SUBROWS].0[index % SUBROWS];
                *slot = (*slot).max(priority(status));
            }
        }
        Self(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_editor_minimap::{ScrollbarParams, draw_scrollbar};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};

    #[test]
    fn isolated_lines_render_as_solid_half_cells_at_their_projected_positions() {
        let rows = ChangeRows::from_lines(10, 400, [(0, 'S'), (160, 'U'), (220, 'D'), (399, 'N')]);
        assert_eq!(
            rows.0.iter().map(|row| row.symbol()).collect::<String>(),
            "▀   ▀▄   ▄"
        );
        assert_eq!(rows.row(4).status(), Some('U'));
        assert_eq!(rows.row(5).status(), Some('D'));
    }

    #[test]
    fn whole_changed_lines_scale_as_intervals_in_short_documents() {
        let all = ChangeRows::project(10, 7, |_, _| Some('U'));
        assert!(all.0.iter().all(|row| row.symbol() == '█'));
        let first_three = ChangeRows::from_lines(10, 12, (0..3).map(|line| (line, 'N')));
        assert_eq!(first_three.row(0).symbol(), '█');
        assert_eq!(first_three.row(1).symbol(), '█');
        assert_eq!(first_three.row(2).symbol(), '▀');
        assert_eq!(first_three.row(3).symbol(), ' ');
    }

    #[test]
    fn all_coverage_patterns_use_single_cell_glyphs() {
        for (coverage, symbol) in [([0, 0], ' '), ([3, 0], '▀'), ([0, 2], '▄'), ([3, 2], '█')]
        {
            let row = ChangeRow(coverage);
            assert_eq!(row.symbol(), symbol);
            assert_eq!(unicode_width::UnicodeWidthChar::width(symbol), Some(1));
        }
    }

    #[test]
    fn inverse_queries_match_independent_forward_projection_at_edges_and_large_coordinates() {
        for total in [1, 2, 3, 10, 79, 1000, usize::MAX] {
            let changes = [
                (0, 'S'),
                (total / 2, 'M'),
                (total - 1, 'N'),
                (usize::MAX, 'D'),
            ];
            for height in [0, 1, 2, 3, 7, 60, 100] {
                let actual = ChangeRows::project(height, total, |start, end| {
                    changes
                        .iter()
                        .filter(|&&(line, _)| (start..end).contains(&line.min(total - 1)))
                        .map(|&(_, status)| status)
                        .max_by_key(|&status| priority(status))
                });
                assert_eq!(
                    actual,
                    ChangeRows::from_lines(height, total, changes),
                    "height={height}, total={total}"
                );
            }
        }
        let mut called = false;
        assert!(
            ChangeRows::project(0, 0, |_, _| {
                called = true;
                Some('N')
            })
            .is_empty()
        );
        assert!(!called);
    }

    #[test]
    fn solid_markers_keep_track_and_thumb_colors_across_themes_and_pointer_states() {
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let colors = theme.colors();
            let changes = ChangeRows::from_lines(10, 400, [(160, 'U'), (170, 'D'), (220, 'D')]);
            for scroll in [0, 160] {
                for (hovered, active) in [(false, false), (true, false), (false, true)] {
                    let (track, thumb) =
                        colors
                            .ui
                            .scrollbar_colors(colors.ui.bg_overview, hovered, active);
                    for width in [1, 2] {
                        let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
                        terminal
                            .draw(|frame| {
                                draw_scrollbar(
                                    frame,
                                    ScrollbarParams {
                                        area: Rect::new(0, 0, width, 10),
                                        total_lines: 400,
                                        visible_lines: 80,
                                        scroll,
                                        search_matches: &[],
                                        diagnostic_lines: &[],
                                        diagnostic_index: None,
                                        change_rows: &changes,
                                        cursor_line: 399,
                                        colors,
                                        hovered,
                                        active,
                                    },
                                )
                            })
                            .unwrap();
                        for x in 0..width {
                            for (y, symbol, color) in [
                                (4, "▀", colors.ui.fg_overview_modified),
                                (5, "▄", colors.ui.fg_overview_deleted),
                            ] {
                                let cell = &terminal.backend().buffer()[(x, y)];
                                assert_eq!(cell.symbol(), symbol);
                                assert_eq!(cell.fg, color);
                                assert_eq!(cell.bg, if scroll == 0 { track } else { thumb });
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn preparation_work_is_bounded_by_track_height_even_for_huge_documents() {
        let mut queries = 0;
        let rows = ChangeRows::project(40, usize::MAX, |_, _| {
            queries += 1;
            Some('U')
        });
        assert_eq!(queries, 40 * SUBROWS);
        assert_eq!(std::mem::size_of_val(rows.0.as_slice()), 80);
    }
}
