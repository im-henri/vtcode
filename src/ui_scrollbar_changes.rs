//! Fixed-resolution change coverage, separate from the text minimap. Four
//! vertical samples per terminal cell preserve small edits and gaps without
//! scanning a document or allocating per marker during scrolling.
pub(crate) const SUBROWS: usize = 4;

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
        let mask = self
            .0
            .into_iter()
            .enumerate()
            .fold(0u8, |mask, (index, status)| {
                mask | (u8::from(status != 0) << index)
            });
        match mask {
            0 => ' ',
            0b0011 => '▀',
            0b1100 => '▄',
            0b1111 => '█',
            // Braille offers four independently addressable vertical rows in
            // one standard-width terminal cell, including gaps within a cell.
            _ => {
                let dots = [0x09, 0x12, 0x24, 0xc0]
                    .into_iter()
                    .enumerate()
                    .filter(|(index, _)| mask & (1 << index) != 0)
                    .fold(0, |dots, (_, row)| dots | row);
                char::from_u32(0x2800 + dots).unwrap()
            }
        }
    }
}

impl ChangeRows {
    pub(crate) fn row(&self, index: usize) -> ChangeRow {
        self.0.get(index).copied().unwrap_or_default()
    }

    /// Query source-line intervals intersecting each quarter cell. Treating a
    /// changed line as an interval, rather than a point, also fills proportional
    /// spans correctly when the scrollbar is taller than one quarter of a file.
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
    fn isolated_lines_and_nearby_hunks_keep_gaps_inside_a_terminal_cell() {
        let one = ChangeRows::from_lines(10, 400, [(160, 'U')]);
        assert_eq!(one.row(4).symbol(), '⠉');
        assert_eq!(one.row(4).status(), Some('U'));
        let two = ChangeRows::from_lines(10, 400, [(160, 'U'), (190, 'U')]);
        assert_eq!(two.row(4).0, [3, 0, 0, 3]);
        assert_eq!(two.row(4).symbol(), '⣉');
        assert_ne!(two.row(4).symbol(), '█');
        assert!(
            two.0
                .iter()
                .enumerate()
                .all(|(row, coverage)| row == 4 || coverage.status().is_none())
        );
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
    fn all_coverage_patterns_are_single_cell_glyphs_and_preserve_empty_quarters() {
        for mask in 0u8..16 {
            let row = ChangeRow(std::array::from_fn(|index| {
                if mask & (1 << index) != 0 { 3 } else { 0 }
            }));
            let symbol = row.symbol();
            assert_eq!(unicode_width::UnicodeWidthChar::width(symbol), Some(1));
            match mask {
                0 => assert_eq!(symbol, ' '),
                3 => assert_eq!(symbol, '▀'),
                12 => assert_eq!(symbol, '▄'),
                15 => assert_eq!(symbol, '█'),
                _ => {
                    let bits = symbol as u32 - 0x2800;
                    for (index, dots) in [0x09, 0x12, 0x24, 0xc0].into_iter().enumerate() {
                        assert_eq!(
                            bits & dots == dots,
                            mask & (1 << index) != 0,
                            "mask={mask}, quarter={index}"
                        );
                    }
                }
            }
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
    fn marker_gaps_keep_track_and_thumb_colors_across_themes_and_pointer_states() {
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let colors = theme.colors();
            let changes = ChangeRows::from_lines(10, 400, [(160, 'U'), (190, 'D')]);
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
                            let cell = &terminal.backend().buffer()[(x, 4)];
                            assert_eq!(cell.symbol(), "⣉");
                            assert_eq!(cell.fg, colors.ui.fg_overview_modified);
                            assert_eq!(cell.bg, if scroll == 0 { track } else { thumb });
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
        assert_eq!(std::mem::size_of_val(rows.0.as_slice()), 160);
    }
}
