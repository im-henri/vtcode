use super::*;
use crate::terminal::TerminalEmulator;

const ERROR: &str = "Error in \"/my/work/path/workplace/directory/file.c\", line 4755, column 49: Undeclared variable L2I_FATAL_ERROR_EVSOCK_DMA_INT_ERR.";

#[test]
fn directory_paths_include_trailing_separators_roots_and_parent_directory_spelling() {
    for path in ["src/", "./src", "../src/", "/tmp/project/", "資料/", r"C:\work\", r"C:\", "/", ".", ".."] {
        let parsed = parse(path);
        assert_eq!(parsed.len(), 1, "{path}");
        assert_eq!(parsed[0].location.path, path);
        assert_eq!(parsed[0].location.line, None);
    }
    let parsed = parse("Directory \"my project/src/\"");
    assert_eq!(parsed[0].location.path, "my project/src/");
    assert!(parse("ordinary terminal output").is_empty());
}

#[test]
fn compiler_locations_support_quotes_colons_parentheses_and_plain_paths() {
    for (message, path, line, column) in [
        (ERROR, "/my/work/path/workplace/directory/file.c", Some(4754), 48),
        ("--> src/main.rs:12:7", "src/main.rs", Some(11), 6),
        ("src/main.rs:12: error: bad call", "src/main.rs", Some(11), 0),
        (r"C:\work\source.c(12,7): error", r"C:\work\source.c", Some(11), 6),
        (r"C:\work\source.c(12, 7): error", r"C:\work\source.c", Some(11), 6),
        (r"C:\work\source.c:12:7: error", r"C:\work\source.c", Some(11), 6),
        ("File \"my project/source.py\", line 3, in main", "my project/source.py", Some(2), 0),
        ("Error in 'my file.c', line 17, column 2", "my file.c", Some(16), 1),
        ("(../src/file.c:4:9)", "../src/file.c", Some(3), 8),
        ("read src/main.c", "src/main.c", None, 0),
        ("Makefile:7: error", "Makefile", Some(6), 0),
        ("/tmp/source(12,3).c:14:9", "/tmp/source(12,3).c", Some(13), 8),
        (".env:2:3", ".env", Some(1), 2),
    ] {
        let links = parse(message);
        assert_eq!(links.len(), 1, "{message}");
        assert_eq!(links[0].location, Location { path: path.into(), line, column }, "{message}");
    }
    assert!(parse("hello 1.2.3 error:14 https://example.com/file.c:12:7").is_empty());
}

#[test]
fn wrapped_colored_errors_and_unicode_use_terminal_cell_coordinates() {
    let output = format!("界 α \x1b[31m{ERROR}\x1b[0m\r\nnext");
    let terminal = TerminalEmulator::test_with_output(40, 8, &output);
    let mut cache = Cache::default();
    // The color escape is not part of the grid; Unicode scalars follow grid cells.
    let plain = format!("界 α {ERROR}");
    let offset = plain[..plain.find("file.c").unwrap()].chars().count();
    let index = cache.hit(&terminal.grid, offset / 40, offset % 40).unwrap();
    assert_eq!(cache.links[index].location.path, "/my/work/path/workplace/directory/file.c");
    assert_eq!(cache.links[index].location.line, Some(4754));
    assert_eq!(cache.links[index].location.column, 48);
    assert!(cache.links[index].end.row > cache.links[index].start.row);
    assert_eq!(cache.scans, 1);
    for col in 12..30 { cache.hit(&terminal.grid, 0, col); }
    assert_eq!(cache.scans, 1, "moving along a logical line must reuse detection");
}

#[test]
fn scrollback_links_join_soft_wraps_but_never_hard_newlines() {
    let output = format!("prefix\r\n{ERROR}\r\nsuffix\r\nlast\r\n");
    let mut terminal = TerminalEmulator::test_with_output(60, 3, &output);
    terminal.grid.scroll_view = terminal.grid.scrollback.len();
    let mut cache = Cache::default();
    let index = cache.hit(&terminal.grid, 1, 14).unwrap();
    assert_eq!(cache.links[index].location.line, Some(4754));
    assert_eq!(cache.links[index].location.column, 48);
    let terminal = TerminalEmulator::test_with_output(60, 3, "src/file.c\r\nline 99, column 2");
    let index = cache.hit(&terminal.grid, 0, 2).unwrap();
    assert_eq!(cache.links[index].location.line, None);
}

#[test]
fn row_stamps_reuse_positive_and_negative_results_despite_unrelated_output() {
    for text in ["src/a.c:12:7", "ordinary terminal output"] {
        let mut terminal = TerminalEmulator::test_with_output(80, 3, text);
        let mut cache = Cache::default();
        cache.hit(&terminal.grid, 0, 5);
        for _ in 0..1000 {
            terminal.grid.cells[2][0].ch = 'x';
            cache.hit(&terminal.grid, 0, 5);
        }
        assert_eq!(cache.scans, 1, "unrelated output must not trigger link parsing");
        terminal.grid.cells[0][4].ch = 'b';
        cache.hit(&terminal.grid, 0, 5);
        assert_eq!(cache.scans, 2, "overwritten output must invalidate detection");
    }
}

#[test]
fn pathological_wrapped_lines_and_huge_terminal_widths_are_bounded() {
    let mut grid = TermGrid::new(80, MAX_ROWS + 1);
    grid.wrapped.fill(true);
    let mut cache = Cache::default();
    assert_eq!(cache.hit(&grid, MAX_ROWS, 10), None);
    assert_eq!(cache.scans, 0);
    assert!(cache.text.is_empty());
    let grid = TermGrid::new(MAX_CELLS + 1, 1);
    assert_eq!(cache.hit(&grid, 0, 10), None);
    assert_eq!(cache.scans, 0);
    assert!(cache.text.is_empty());
}
