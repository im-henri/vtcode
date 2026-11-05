use super::*;

fn output(grid: &mut TermGrid, text: &str) {
    grid.advance_output(&mut vte::Parser::new(), text.as_bytes());
}

#[test]
fn finds_scrollback_and_joins_only_soft_wrapped_lines() {
    let mut grid = TermGrid::new(8, 2);
    output(&mut grid, "old match\r\nnext\r\nabcdefghij\r\nlast");
    let mut index = Index::default();
    index.update(&grid, "old match", false);
    assert_eq!(index.matches.len(), 1);
    index.reveal(&mut grid);
    assert!(grid.scroll_view > 0);
    index.update(&grid, "fghij", false);
    assert_eq!(index.matches.len(), 1);
    let hit = index.matches[0];
    assert!(hit.start.row < hit.end.row);
    index.update(&grid, "nextabc", false);
    assert!(index.matches.is_empty(), "hard newlines are not joined");
}

#[test]
fn literal_case_and_unicode_matches_map_to_terminal_columns() {
    let mut grid = TermGrid::new(40, 3);
    output(&mut grid, "界 café [A-Z] CAFÉ");
    let mut index = Index::default();
    index.update(&grid, "café", false);
    assert_eq!(index.matches.len(), 2);
    assert_eq!(index.matches[0].start.column, 2);
    assert_eq!(index.matches[0].end.column, 6);
    index.update(&grid, "café", true);
    assert_eq!(index.matches.len(), 1);
    index.update(&grid, "[A-Z]", false);
    assert_eq!(index.matches.len(), 1, "search is literal");
    index.update(&grid, "界", false);
    assert_eq!(index.matches[0].end.column, 1);
    grid.cells[1][0].ch = '界';
    grid.cells[1][1].ch = '\0';
    grid.text_changed();
    index.update(&grid, "界", false);
    assert_eq!(
        index.matches[1].end.column, 2,
        "include explicit wide-cell continuations"
    );
}

#[test]
fn reuses_unchanged_rows_and_tracks_updates_clear_and_alternate_screen() {
    let mut grid = TermGrid::new(30, 4);
    output(&mut grid, "match\r\nmatch\r\nother");
    let mut index = Index::default();
    assert!(index.update(&grid, "match", false));
    let scanned = index.rescanned;
    assert!(!index.update(&grid, "match", false));
    assert_eq!(index.rescanned, scanned);
    output(&mut grid, " match");
    assert!(index.update(&grid, "match", false));
    assert_eq!(index.rescanned, scanned + 1);
    assert_eq!(index.matches.len(), 3);
    output(&mut grid, "\x1b[?1049hother screen");
    index.update(&grid, "match", false);
    assert!(index.matches.is_empty());
    output(&mut grid, "\x1b[?1049l");
    index.update(&grid, "match", false);
    assert_eq!(index.matches.len(), 3);
    output(&mut grid, "\x1b[H\x1b[2J\x1b[3J");
    index.update(&grid, "match", false);
    assert!(index.matches.is_empty());
    output(&mut grid, "new match");
    grid.resize(15, 3);
    index.update(&grid, "match", false);
    assert_eq!(index.matches.len(), 1);
}

#[test]
fn navigation_wraps_and_preserves_match_when_output_scrolls() {
    let mut grid = TermGrid::new(20, 3);
    output(&mut grid, "match\r\nmatch\r\nlast");
    let mut index = Index::default();
    index.update(&grid, "match", false);
    let anchor = index.matches[index.selected.unwrap()].anchor;
    index.navigate(true);
    assert_eq!(index.selected, Some(1));
    index.navigate(false);
    assert_eq!(index.selected, Some(0));
    output(&mut grid, "\r\nnew output");
    index.update(&grid, "match", false);
    assert_eq!(index.matches[index.selected.unwrap()].anchor, anchor);
    index.clear();
    assert!(index.matches.is_empty());
}

#[test]
fn caps_results_at_newest_matches_even_in_one_long_soft_wrapped_line() {
    let mut grid = TermGrid::new(80, 3);
    output(&mut grid, &"a".repeat(MAX_MATCHES + 80));
    let mut index = Index::default();
    index.update(&grid, "a", false);
    assert_eq!(index.matches.len(), MAX_MATCHES);
    assert!(index.limited);
    assert_eq!(index.matches[0].start.row, 1);
    assert_eq!(index.matches[0].start.column, 0);
}
