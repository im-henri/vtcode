//! Scrollbar change coverage uses solid half-cell blocks so even a single
//! changed line stays visible in a long file. Projection is bounded by the
//! scrollbar height.
use crate::line_changes::LineChanges;

const SUBROWS: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ChangeRow([LineChanges; SUBROWS]);

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ChangeRows(Vec<ChangeRow>);

impl ChangeRow {
    pub(crate) fn halves(self) -> [LineChanges; SUBROWS] {
        self.0
    }

    pub(crate) fn changes(self) -> LineChanges {
        self.0[0].merge(self.0[1])
    }

    pub(crate) fn symbol(self) -> char {
        let [upper, lower] = self.0;
        coverage_symbol(!upper.is_empty(), !lower.is_empty(), false)
    }

    /// Keep the left half free for navigation in a one-column scrollbar,
    /// retaining the same half-row precision as the full-width markers.
    pub(crate) fn narrow_symbol(self) -> char {
        let [upper, lower] = self.0;
        coverage_symbol(!upper.is_empty(), !lower.is_empty(), true)
    }
}

pub(super) fn coverage_symbol(upper: bool, lower: bool, narrow: bool) -> char {
    match (upper, lower, narrow) {
        (true, false, false) => '▀',
        (false, true, false) => '▄',
        (true, true, false) => '█',
        (true, false, true) => '▝',
        (false, true, true) => '▗',
        (true, true, true) => '▐',
        _ => ' ',
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
        mut query: impl FnMut(usize, usize) -> LineChanges,
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
                *slot = query(start, end);
            }
        }
        Self(rows)
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.iter().all(|row| row.changes().is_empty())
    }

    /// Independent forward projection for reference tests. Production uses
    /// indexed range queries above and is bounded by scrollbar height.
    #[cfg(test)]
    pub(crate) fn from_lines<T: Into<LineChanges>>(
        height: usize,
        total: usize,
        lines: impl IntoIterator<Item = (usize, T)>,
    ) -> Self {
        let mut rows = vec![ChangeRow::default(); height];
        let samples = height * SUBROWS;
        let total = total.max(1);
        for (line, status) in lines {
            let change = status.into();
            let line = line.min(total - 1);
            let start = (line as u128 * samples as u128 / total as u128) as usize;
            let end = (((line + 1) as u128 * samples as u128).div_ceil(total as u128)) as usize;
            for index in start..end {
                let slot = &mut rows[index / SUBROWS].0[index % SUBROWS];
                *slot = slot.merge(change);
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
        assert_eq!(rows.row(4).changes(), 'U'.into());
        assert_eq!(rows.row(5).changes(), 'D'.into());
    }

    #[test]
    fn whole_changed_lines_scale_as_intervals_in_short_documents() {
        let all = ChangeRows::project(10, 7, |_, _| 'U'.into());
        assert!(all.0.iter().all(|row| row.symbol() == '█'));
        let first_three = ChangeRows::from_lines(10, 12, (0..3).map(|line| (line, 'N')));
        assert_eq!(first_three.row(0).symbol(), '█');
        assert_eq!(first_three.row(1).symbol(), '█');
        assert_eq!(first_three.row(2).symbol(), '▀');
        assert_eq!(first_three.row(3).symbol(), ' ');
    }

    #[test]
    fn all_coverage_patterns_use_single_cell_glyphs() {
        for (coverage, symbol, narrow) in [([0, 0], ' ', ' '), ([3, 0], '▀', '▝'),
            ([0, 2], '▄', '▗'), ([3, 2], '█', '▐')]
        {
            let row = ChangeRow(coverage.map(LineChanges::from_bits));
            assert_eq!(row.symbol(), symbol);
            assert_eq!(row.narrow_symbol(), narrow);
            assert_eq!(unicode_width::UnicodeWidthChar::width(symbol), Some(1));
            assert_eq!(unicode_width::UnicodeWidthChar::width(narrow), Some(1));
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
                        .fold(LineChanges::default(), |combined, &(_, status)| combined.merge(status.into()))
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
                LineChanges::UNSAVED
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
            let changes = ChangeRows::from_lines(10, 400, [(160, 'M'), (170, 'D'), (220, 'D')]);
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
                                let surface = if scroll == 0 { track } else { thumb };
                                if width == 2 && x == 0 {
                                    assert_eq!(cell.fg, surface);
                                    assert_eq!(cell.bg, surface);
                                    continue;
                                }
                                assert_eq!(cell.symbol(), if width == 1 {
                                    if symbol == "▀" { "▝" } else { "▗" }
                                } else { symbol });
                                assert_eq!(cell.fg, color);
                                assert_eq!(cell.bg, if width == 1 { surface } else { track });
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn preparation_work_is_bounded_by_track_height_even_for_huge_documents() {
        let height = 40;
        let mut queries = 0;
        let rows = ChangeRows::project(height, usize::MAX, |_, _| {
            queries += 1;
            'U'.into()
        });
        assert_eq!(queries, height * SUBROWS);
        assert_eq!(rows.0.len(), height);
        assert!(std::mem::size_of::<ChangeRow>() <= SUBROWS * std::mem::size_of::<u16>());
    }
}
