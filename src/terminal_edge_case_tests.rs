use super::*;

fn feed(grid: &mut TermGrid, text: &str) {
    let mut parser = vte::Parser::new();
    for byte in text.bytes() {
        parser.advance(grid, byte);
    }
}

fn populated_grid() -> TermGrid {
    let mut grid = TermGrid::new(4, 6);
    feed(&mut grid, "A\r\nB\r\nC\r\nD\r\nE\r\nF");
    grid.wrapped = vec![true, false, true, false, true, false];
    grid
}

fn rows(grid: &TermGrid) -> String {
    grid.cells.iter().map(|row| row[0].ch).collect()
}

#[test]
fn line_edits_outside_scroll_margins_are_ignored() {
    for action in ['L', 'M'] {
        for row in [1, 5, 6] {
            let mut grid = populated_grid();
            let wrapped = grid.wrapped.clone();
            feed(&mut grid, &format!("\x1b[2;4r\x1b[{row};1H\x1b[2{action}"));
            assert_eq!(rows(&grid), "ABCDEF", "{action} on row {row}");
            assert_eq!(grid.wrapped, wrapped);
            assert!(grid.scrollback.is_empty());
        }
    }
}

#[test]
fn ignored_line_edits_do_not_clear_a_selection() {
    for action in ['L', 'M'] {
        let mut grid = populated_grid();
        feed(&mut grid, "\x1b[2;4r\x1b[1;1H");
        grid.begin_selection(0, 0);
        grid.extend_selection(1, 0);
        grid.finish_selection();
        let selected = grid.selected_text();
        feed(&mut grid, &format!("\x1b[{action}"));
        assert_eq!(grid.selected_text(), selected);
        assert!(grid.has_selection());
    }
}

#[test]
fn line_edits_inside_scroll_margins_preserve_headers_and_footers() {
    for (action, expected, wrapped) in [
        ('L', "AB CEF", [true, false, false, true, true, false]),
        ('M', "ABD EF", [true, false, false, false, true, false]),
    ] {
        let mut grid = populated_grid();
        feed(&mut grid, &format!("\x1b[2;4r\x1b[3;1H\x1b[{action}"));
        assert_eq!(rows(&grid), expected);
        assert_eq!(grid.wrapped, wrapped);
        assert!(grid.scrollback.is_empty());
    }
}

#[test]
fn oversized_line_edits_only_clear_the_remaining_scroll_region() {
    for action in ['L', 'M'] {
        let mut grid = populated_grid();
        feed(&mut grid, &format!("\x1b[2;4r\x1b[3;1H\x1b[65535{action}"));
        assert_eq!(rows(&grid), "AB  EF");
        assert_eq!(grid.wrapped, [true, false, false, false, true, false]);
    }
}

#[test]
fn linefeed_below_the_scroll_region_advances_without_scrolling() {
    let mut grid = populated_grid();
    feed(&mut grid, "\x1b[2;4r\x1b[5;2H\n");
    assert_eq!((grid.cursor_row, grid.cursor_col), (5, 1));
    feed(&mut grid, "\n");
    assert_eq!((grid.cursor_row, grid.cursor_col), (5, 1));
    assert_eq!(rows(&grid), "ABCDEF");
    assert!(grid.scrollback.is_empty());
}

#[test]
fn scrolling_a_region_below_a_header_does_not_pollute_history() {
    for sequence in ["\x1b[2;4r\x1b[4;1H\n", "\x1b[2;4r\x1b[S"] {
        let mut grid = populated_grid();
        feed(&mut grid, sequence);
        assert_eq!(rows(&grid), "ACD EF");
        assert_eq!(grid.wrapped, [true, true, false, false, true, false]);
        assert!(grid.scrollback.is_empty());
        assert!(grid.scrollback_wrapped.is_empty());
    }
}

#[test]
fn scrolling_the_full_viewport_still_saves_history() {
    let mut grid = populated_grid();
    feed(&mut grid, "\x1b[6;1H\n");
    assert_eq!(rows(&grid), "BCDEF ");
    assert_eq!(grid.scrollback.len(), 1);
    assert_eq!(grid.scrollback[0][0].ch, 'A');
    assert_eq!(grid.scrollback_wrapped, [true]);
}

#[test]
fn scrolling_with_a_fixed_footer_still_saves_rows_leaving_the_screen() {
    let mut grid = populated_grid();
    feed(&mut grid, "\x1b[1;4r\x1b[S");
    assert_eq!(rows(&grid), "BCD EF");
    assert_eq!(grid.scrollback.len(), 1);
    assert_eq!(grid.scrollback[0][0].ch, 'A');
    assert_eq!(grid.scrollback_wrapped, [true]);
}

#[test]
fn oversized_scroll_requests_do_not_fill_history_with_blank_lines() {
    let mut grid = populated_grid();
    feed(&mut grid, "\x1b[65535S");
    assert_eq!(rows(&grid), "      ");
    assert_eq!(grid.scrollback.len(), grid.height);
    assert_eq!(
        grid.scrollback
            .iter()
            .map(|row| row[0].ch)
            .collect::<String>(),
        "ABCDEF"
    );
    feed(&mut grid, "\x1b[65535T");
    assert_eq!(rows(&grid), "      ");
    assert_eq!(grid.scrollback.len(), grid.height);
}

#[test]
fn partial_scroll_preserves_selection_in_the_fixed_header() {
    let mut grid = populated_grid();
    feed(&mut grid, "\x1b[2;4r");
    grid.begin_selection(0, 0);
    grid.extend_selection(0, 1);
    grid.finish_selection();
    let selected = grid.selected_text();
    feed(&mut grid, "\x1b[S");
    assert_eq!(grid.selected_text(), selected);
    assert!(grid.has_selection());
    assert!(grid.selection_contains(0, 0));
}

#[test]
fn erase_saved_lines_clears_history_without_erasing_the_screen() {
    let mut grid = populated_grid();
    feed(&mut grid, "\n");
    let screen = rows(&grid);
    let wrapped = grid.wrapped.clone();
    let cursor = (grid.cursor_row, grid.cursor_col);
    grid.scroll_view = 1;
    grid.begin_selection(0, 0);
    grid.extend_selection(0, 1);
    grid.finish_selection();
    feed(&mut grid, "\x1b[3J");
    assert!(grid.scrollback.is_empty());
    assert!(grid.scrollback_wrapped.is_empty());
    assert_eq!(grid.scroll_view, 0);
    assert!(!grid.has_selection());
    assert_eq!(rows(&grid), screen);
    assert_eq!(grid.wrapped, wrapped);
    assert_eq!((grid.cursor_row, grid.cursor_col), cursor);
}

#[test]
fn erase_saved_lines_preserves_a_selection_on_the_live_screen() {
    let mut grid = populated_grid();
    feed(&mut grid, "\n");
    grid.begin_selection(0, 0);
    grid.extend_selection(1, 0);
    grid.finish_selection();
    let selected = grid.selected_text();
    feed(&mut grid, "\x1b[3J");
    assert_eq!(grid.selected_text(), selected);
    assert!(grid.selection_contains(0, 0));
    assert!(grid.scrollback.is_empty());
}

#[test]
fn erase_screen_keeps_saved_lines() {
    let mut grid = populated_grid();
    feed(&mut grid, "\n\x1b[2J");
    assert_eq!(rows(&grid), "      ");
    assert_eq!(grid.scrollback.len(), 1);
    assert_eq!(grid.scrollback[0][0].ch, 'A');
}
