use super::*;

fn feed(grid: &mut TermGrid, text: &str) {
    let mut parser = vte::Parser::new();
    for byte in text.bytes() {
        parser.advance(grid, byte);
    }
}

fn select(grid: &mut TermGrid, start: (usize, usize), end: (usize, usize)) {
    grid.begin_selection(start.0, start.1);
    grid.extend_selection(end.0, end.1);
    grid.finish_selection();
}

#[test]
fn forward_and_reverse_selections_copy_unicode_without_ansi_or_padding() {
    let mut grid = TermGrid::new(20, 4);
    feed(&mut grid, "  héllo \x1b[31mworld\x1b[0m\r\nnext");
    for (start, end) in [((0, 2), (1, 3)), ((1, 3), (0, 2))] {
        select(&mut grid, start, end);
        assert_eq!(grid.selected_text().as_deref(), Some("héllo world\nnext"));
        assert!(!grid.selection_contains(0, 1));
        assert!(grid.selection_contains(0, 2));
        assert!(grid.selection_contains(1, 3));
        assert!(!grid.selection_contains(1, 4));
    }
}

#[test]
fn soft_wraps_join_without_losing_spaces_or_hard_newlines() {
    let mut grid = TermGrid::new(6, 4);
    feed(&mut grid, "ab  cdEF\r\nlast");
    select(&mut grid, (0, 0), (2, 3));
    assert_eq!(grid.selected_text().as_deref(), Some("ab  cdEF\nlast"));
    select(&mut grid, (0, 2), (1, 0));
    assert_eq!(grid.selected_text().as_deref(), Some("  cdE"));
}

#[test]
fn blank_lines_survive_but_terminal_padding_is_not_copied() {
    let mut grid = TermGrid::new(10, 4);
    feed(&mut grid, "one\r\n\r\ntwo");
    select(&mut grid, (0, 0), (2, 9));
    assert_eq!(grid.selected_text().as_deref(), Some("one\n\ntwo"));
}

#[test]
fn a_click_is_focus_only_and_zero_sized_grids_cannot_select() {
    let mut grid = TermGrid::new(10, 2);
    grid.begin_selection(0, 0);
    grid.finish_selection();
    assert!(!grid.has_selection());
    assert_eq!(grid.selected_text(), None);
    let mut empty = TermGrid::new(0, 0);
    select(&mut empty, (usize::MAX, usize::MAX), (0, 0));
    assert!(!empty.has_selection());
    assert!(!empty.selection_contains(0, 0));
}

#[test]
fn history_selection_and_viewport_survive_new_output_and_styles() {
    let mut grid = TermGrid::new(10, 3);
    feed(&mut grid, "zero\r\none\r\ntwo\r\nthree");
    grid.scroll_view = 1;
    select(&mut grid, (0, 0), (1, 2));
    assert_eq!(grid.selected_text().as_deref(), Some("zero\none"));
    feed(&mut grid, "\r\nfour\x1b[31m\x1b[0m");
    assert_eq!(grid.scroll_view, 2);
    assert_eq!(grid.selected_text().as_deref(), Some("zero\none"));
    assert_eq!(grid.display_line(0)[0].ch, 'z');
    assert!(grid.selection_contains(0, 0));
    grid.scroll_view = 0;
    assert_eq!(grid.selected_text().as_deref(), Some("zero\none"));
    assert!(!grid.selection_contains(0, 0));
}

#[test]
fn rewritten_rows_clear_selection_but_other_rows_do_not() {
    let mut grid = TermGrid::new(10, 3);
    feed(&mut grid, "first\r\nsecond");
    select(&mut grid, (0, 0), (0, 4));
    feed(&mut grid, "!");
    assert_eq!(grid.selected_text().as_deref(), Some("first"));
    feed(&mut grid, "\x1b[1;1Hreplace");
    assert_eq!(grid.selected_text(), None);
    select(&mut grid, (0, 0), (0, 1));
    feed(&mut grid, "\x1b[2J");
    assert_eq!(grid.selected_text(), None);
}

#[test]
fn reflow_screen_switches_and_partial_scrolls_do_not_retarget_selections() {
    let mut grid = TermGrid::new(10, 3);
    feed(&mut grid, "first\r\nsecond");
    select(&mut grid, (0, 0), (0, 4));
    grid.resize(10, 3);
    assert!(grid.has_selection());
    grid.resize(5, 3);
    assert_eq!(grid.selected_text(), None);
    select(&mut grid, (0, 0), (0, 1));
    feed(&mut grid, "\x1b[?1049hother");
    assert_eq!(grid.selected_text(), None);
    select(&mut grid, (0, 0), (0, 1));
    feed(&mut grid, "\x1b[?1049l");
    assert_eq!(grid.selected_text(), None);
    select(&mut grid, (0, 0), (0, 1));
    feed(&mut grid, "\x1b[2;3r\x1b[S");
    // The fixed header stays put and is not added to scrollback. Its
    // selection must survive, while a selection in the moving rows clears.
    assert_eq!(grid.selected_text().as_deref(), Some("fi"));
    select(&mut grid, (1, 0), (1, 1));
    feed(&mut grid, "\x1b[S");
    assert_eq!(grid.selected_text(), None);
}

#[test]
fn history_eviction_shifts_surviving_selections_and_clears_evicted_ones() {
    let mut grid = TermGrid::new(12, 2);
    for index in 0..5002 {
        feed(&mut grid, &format!("{index:05}\r\n"));
    }
    assert_eq!(grid.scrollback.len(), 5000);
    grid.scroll_view = 5000;
    select(&mut grid, (0, 0), (0, 4));
    feed(&mut grid, "more\r\n");
    assert_eq!(grid.selected_text(), None);
    grid.scroll_view = 4990;
    select(&mut grid, (0, 0), (1, 4));
    let before = grid.selected_text();
    feed(&mut grid, "next\r\n");
    assert_eq!(grid.selected_text(), before);
    assert!(grid.selection_contains(0, 0));
}

#[test]
fn clearing_live_screen_keeps_immutable_history_selection() {
    let mut grid = TermGrid::new(10, 2);
    feed(&mut grid, "history\r\nmore\r\nnow");
    grid.scroll_view = 1;
    select(&mut grid, (0, 0), (0, 3));
    feed(&mut grid, "\x1b[2J");
    assert_eq!(grid.selected_text().as_deref(), Some("hist"));
    assert_eq!(grid.scroll_view, 1);
}
