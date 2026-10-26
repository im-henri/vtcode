use super::*;
use std::hint::black_box;
use std::time::Instant;

#[path = "terminal_scroll_reference.rs"]
mod reference;

fn history_grid(width: usize, height: usize) -> TermGrid {
    let mut grid = TermGrid::new(width, height);
    for index in 0..5000 {
        let mut row = vec![TermCell::default(); width];
        for (column, cell) in row.iter_mut().take(width / 2).enumerate() {
            cell.ch = char::from(b'a' + ((index + column) % 26) as u8);
        }
        grid.scrollback.push_back(row.into());
        grid.scrollback_wrapped.push_back(index % 3 == 0);
    }
    grid
}

fn fixture(width: usize, height: usize, history: usize, alternate: bool) -> TermGrid {
    let mut grid = TermGrid::new(width, height);
    if alternate {
        grid.enter_alt_screen();
    }
    for (index, row) in grid.cells.iter_mut().enumerate() {
        for cell in row {
            cell.ch = char::from(b'A' + index as u8);
            cell.fg = Color::Indexed(index as u8);
            cell.bold = index % 2 == 0;
        }
        grid.wrapped[index] = index % 2 == 0;
    }
    for index in 0..history {
        grid.scrollback.push_back(vec![
            TermCell {
                ch: char::from(b'a' + (index % 26) as u8),
                ..TermCell::default()
            };
            width
        ].into());
        grid.scrollback_wrapped.push_back(index % 3 == 0);
    }
    grid
}

fn assert_same(actual: &TermGrid, expected: &TermGrid) {
    assert_eq!(actual.cells, expected.cells);
    assert_eq!(actual.wrapped, expected.wrapped);
    assert_eq!(actual.scrollback, expected.scrollback);
    assert_eq!(actual.scrollback_wrapped, expected.scrollback_wrapped);
    assert_eq!(actual.scroll_view, expected.scroll_view);
    assert_eq!(
        (actual.cursor_row, actual.cursor_col),
        (expected.cursor_row, expected.cursor_col)
    );
    assert_eq!(actual.selected_text(), expected.reference_selected_text());
    assert_eq!(actual.has_selection(), expected.has_selection());
    for row in 0..actual.height {
        let columns = actual.selection_columns(row);
        for column in 0..actual.width {
            assert_eq!(
                columns.contains(&column),
                expected.selection_contains(row, column)
            );
        }
    }
}

#[test]
fn bulk_scrolling_matches_reference_across_regions_and_counts() {
    for width in [0, 1, 7] {
        for height in [1, 4, 9] {
            for top in 0..height {
                for bottom in top..height {
                    for alternate in [false, true] {
                        for count in [0, 1, 2, height, usize::MAX] {
                            for down in [false, true] {
                                let mut actual = fixture(width, height, 3, alternate);
                                let mut expected = fixture(width, height, 3, alternate);
                                for grid in [&mut actual, &mut expected] {
                                    grid.scroll_top = top;
                                    grid.scroll_bottom = bottom;
                                    grid.scroll_view = 2;
                                }
                                if down {
                                    actual.scroll_down(count);
                                    expected.reference_scroll_down(count);
                                } else {
                                    actual.scroll_up(count);
                                    expected.reference_scroll_up(count);
                                }
                                assert_same(&actual, &expected);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut empty = TermGrid::new(0, 0);
    empty.scroll_up(usize::MAX);
    empty.scroll_down(usize::MAX);
    assert!(empty.cells.is_empty());
}

#[test]
fn history_recycling_preserves_eviction_selection_and_conpty_accounting() {
    for history in [4998, 5000] {
        for view in [0, 2, 4990, history] {
            for bottom in [3, 5] {
                for selection in [false, true] {
                    let mut actual = fixture(8, 6, history, false);
                    let mut expected = fixture(8, 6, history, false);
                    for grid in [&mut actual, &mut expected] {
                        grid.scroll_view = view;
                        grid.scroll_bottom = bottom;
                        if selection {
                            grid.begin_selection(0, 0);
                            grid.extend_selection(1, 4);
                            grid.finish_selection();
                        }
                        grid.conpty_resize = Some(ConptyResize::Repainting {
                            cells: Vec::new(),
                            wrapped: Vec::new(),
                            scrolled_rows: 7,
                        });
                    }
                    actual.scroll_up(6);
                    expected.reference_scroll_up(6);
                    assert_same(&actual, &expected);
                    let count = |grid: &TermGrid| match grid.conpty_resize {
                        Some(ConptyResize::Repainting { scrolled_rows, .. }) => scrolled_rows,
                        _ => 0,
                    };
                    assert_eq!(count(&actual), count(&expected));
                    assert_eq!(actual.scrollback.len(), 5000);
                }
            }
        }
    }
}

#[test]
fn terminal_scrolling_and_erasure_reuse_existing_row_allocations() {
    let mut grid = fixture(20, 6, 0, true);
    let mut addresses: Vec<_> = grid.cells.iter().map(|row| row.as_ptr()).collect();
    addresses.sort();
    grid.scroll_up(4);
    grid.scroll_down(3);
    grid.erase_in_display(2);
    grid.erase_in_line(2);
    let mut after: Vec<_> = grid.cells.iter().map(|row| row.as_ptr()).collect();
    after.sort();
    assert_eq!(addresses, after);
    let mut grid = fixture(20, 6, 5000, false);
    let evicted_storage = grid.scrollback[0].as_ptr();
    grid.scroll_up(1);
    assert_eq!(grid.cells.last().unwrap().as_ptr(), evicted_storage);
    assert!(
        grid.cells
            .last()
            .unwrap()
            .iter()
            .all(|cell| *cell == TermCell::default())
    );
}

#[test]
fn display_and_line_erasure_match_reference_and_preserve_history() {
    for display in [true, false] {
        for mode in [0, 1, 2, 3, 99] {
            for row in 0..6 {
                for col in [0, 4, 7] {
                    let mut actual = fixture(8, 6, 3, false);
                    let mut expected = fixture(8, 6, 3, false);
                    for grid in [&mut actual, &mut expected] {
                        grid.cursor_row = row;
                        grid.cursor_col = col;
                        grid.begin_selection(0, 2);
                        grid.extend_selection(2, 5);
                        grid.finish_selection();
                    }
                    if display {
                        actual.erase_in_display(mode);
                        expected.reference_erase_in_display(mode);
                    } else {
                        actual.erase_in_line(mode);
                        expected.reference_erase_in_line(mode);
                    }
                    assert_same(&actual, &expected);
                }
            }
        }
    }
}

#[test]
fn copied_text_and_selection_rows_match_reference_for_unicode_and_history() {
    let mut grid = fixture(8, 6, 4, false);
    for (index, row) in grid
        .scrollback
        .iter_mut()
        .chain(&mut grid.cells)
        .enumerate()
    {
        for (column, cell) in row.iter_mut().enumerate() {
            cell.ch = [' ', '\0', 'é', '界', '😀', '\t', ' ', ' '][(column + index) % 8];
        }
    }
    for start_view in [0, 2, 4] {
        for end_view in [0, 2, 4] {
            for start in [(0, 0), (2, 4), (5, 7)] {
                for end in [(0, 0), (2, 4), (5, 7)] {
                    grid.scroll_view = start_view;
                    grid.begin_selection(start.0, start.1);
                    grid.scroll_view = end_view;
                    grid.extend_selection(end.0, end.1);
                    grid.finish_selection();
                    assert_eq!(grid.selected_text(), grid.reference_selected_text());
                    for row in 0..6 {
                        for col in 0..8 {
                            assert_eq!(
                                grid.selection_columns(row).contains(&col),
                                grid.selection_contains(row, col)
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_terminal_scroll_and_clear() {
    for workload in ["history", "region", "clear", "copy"] {
        let mut samples = [Vec::new(), Vec::new()];
        for run in 0..9 {
            for reference in if run % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let mut grid = history_grid(160, 60);
                if workload == "region" {
                    grid.enter_alt_screen();
                }
                if workload == "copy" {
                    grid.scroll_view = 5000;
                    grid.begin_selection(0, 0);
                    grid.scroll_view = 0;
                    grid.extend_selection(59, 159);
                    grid.finish_selection();
                }
                let iterations = if workload == "copy" { 20 } else { 2000 };
                let start = Instant::now();
                for _ in 0..iterations {
                    match workload {
                        "history" if reference => grid.reference_scroll_up(1),
                        "history" => grid.scroll_up(1),
                        "region" if reference => {
                            grid.reference_scroll_up(40);
                            grid.reference_scroll_down(40);
                        }
                        "region" => {
                            grid.scroll_up(40);
                            grid.scroll_down(40);
                        }
                        "clear" if reference => grid.reference_erase_in_display(2),
                        "clear" => grid.erase_in_display(2),
                        _ if reference => {
                            black_box(grid.reference_selected_text());
                        }
                        _ => {
                            black_box(grid.selected_text());
                        }
                    }
                    black_box(&grid);
                }
                if run >= 2 {
                    samples[usize::from(!reference)].push(start.elapsed() / iterations);
                }
            }
        }
        for sample in &mut samples {
            sample.sort();
        }
        eprintln!(
            "terminal_{workload} before={:?} after={:?}",
            samples[0][3], samples[1][3]
        );
    }
}
