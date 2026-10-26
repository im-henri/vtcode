use super::*;

fn feed(grid: &mut TermGrid, text: &str) {
    let mut parser = vte::Parser::new();
    for byte in text.bytes() {
        parser.advance(grid, byte);
    }
}

fn row_text(row: &[TermCell]) -> String {
    row.iter()
        .map(|cell| cell.ch)
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn shorter_pane_keeps_output_above_the_cursor_instead_of_blank_bottom_rows() {
    let mut grid = TermGrid::new(40, 24);
    feed(&mut grid, "first log\r\nsecond log\r\n> ");
    grid.resize(40, 6);
    assert_eq!(row_text(grid.display_line(0)), "first log");
    assert_eq!(row_text(grid.display_line(1)), "second log");
    assert_eq!((grid.cursor_row, grid.cursor_col), (2, 2));
    feed(&mut grid, "echo");
    assert_eq!(row_text(grid.display_line(2)), "> echo");
}

#[test]
fn taller_pane_restores_scrollback_and_keeps_the_prompt_attached() {
    let mut grid = TermGrid::new(30, 12);
    for line in 0..10 {
        feed(&mut grid, &format!("log {line}\r\n"));
    }
    feed(&mut grid, "> ");
    grid.resize(30, 4);
    assert_eq!(row_text(grid.display_line(2)), "log 9");
    assert_eq!((grid.cursor_row, grid.cursor_col), (3, 2));
    grid.resize(30, 12);
    for line in 0..10 {
        assert_eq!(row_text(grid.display_line(line)), format!("log {line}"));
    }
    assert_eq!((grid.cursor_row, grid.cursor_col), (10, 2));
    assert!(grid.scrollback.is_empty());
}

#[test]
fn width_changes_reflow_long_logs_without_losing_text_or_styles() {
    let mut grid = TermGrid::new(24, 10);
    feed(
        &mut grid,
        "\x1b[31;1mabcdefghijklmnopqrstuvwxyz0123456789\x1b[0m\r\n> ",
    );
    for (width, height) in [(8, 4), (60, 6), (12, 10), (60, 8)] {
        grid.resize(width, height);
        let text: String = grid
            .scrollback
            .iter()
            .chain(&grid.cells)
            .map(|row| row_text(row))
            .collect();
        assert_eq!(text, "abcdefghijklmnopqrstuvwxyz0123456789>");
        let a = grid
            .scrollback
            .iter()
            .chain(&grid.cells)
            .flatten()
            .find(|cell| cell.ch == 'a')
            .unwrap();
        assert_eq!(a.fg, Color::Red);
        assert!(a.bold);
    }
    assert_eq!(
        row_text(grid.display_line(0)),
        "abcdefghijklmnopqrstuvwxyz0123456789"
    );
    assert_eq!((grid.cursor_row, grid.cursor_col), (1, 2));
    feed(&mut grid, "next");
    assert_eq!(row_text(grid.display_line(1)), "> next");
}

#[test]
fn unwrapped_terminal_rows_reuse_storage_while_resizing_and_keep_styles() {
    let mut grid = TermGrid::new(120, 4);
    feed(&mut grid, "\x1b[31;1mfirst\x1b[0m\r\nsecond\r\nthird\r\n> ");
    let row_storage = grid.cells[0].as_ptr();
    grid.resize(119, 4);
    assert_eq!(
        grid.cells[0].as_ptr(),
        row_storage,
        "short hard-ended rows reuse their allocation"
    );
    assert_eq!(row_text(&grid.cells[0]), "first");
    assert_eq!(grid.cells[0][0].fg, Color::Red);
    assert!(grid.cells[0][0].bold);
    assert_eq!((grid.cursor_row, grid.cursor_col), (3, 2));
    assert_eq!(grid.wrapped, [false; 4]);
    for width in [118, 100, 80, 120, 30] {
        grid.resize(width, 4);
        assert_eq!(row_text(&grid.cells[0]), "first");
        assert_eq!(row_text(&grid.cells[3]), "> ".trim_end());
        assert_eq!(grid.cells[0][0].fg, Color::Red);
    }
}

#[test]
fn conpty_height_growth_keeps_history_safe_from_viewport_repaint() {
    for width in [90, 20] {
        let mut grid = TermGrid::new(32, 20);
        grid.is_conpty = true;
        feed(&mut grid, "\x1bc");
        for line in 0..6 {
            feed(&mut grid, &format!("PRIMARY_LOG_{line:02}\r\n"));
        }
        feed(
            &mut grid,
            "WRAPabcdefghijklmnopqrstuvwxyz0123456789END\r\nRESIZE_READY\r\n",
        );
        grid.resize(90, 8);

        // Model ConPTY's first repaint and input acknowledgement. Older rows
        // have left its viewport and will not be included in later repaints.
        feed(&mut grid, "\x1b[2J\x1b[H");
        for line in 1..6 {
            feed(&mut grid, &format!("PRIMARY_LOG_{line:02}\r\n"));
        }
        feed(
            &mut grid,
            "WRAPabcdefghijklmnopqrstuvwxyz0123456789END\r\nRESIZE_READY\r\nMOVE_0\r\nACK_MOVE_0\r\n",
        );

        grid.resize(width, 25);
        feed(&mut grid, "\x1b[2J\x1b[H");
        for line in 3..6 {
            feed(&mut grid, &format!("PRIMARY_LOG_{line:02}\r\n"));
        }
        feed(
            &mut grid,
            "WRAPabcdefghijklmnopqrstuvwxyz0123456789END\r\nRESIZE_READY\r\nMOVE_0\r\nACK_MOVE_0\r\nMOVE_1\r\nACK_MOVE_1\r\n",
        );

        let output: String = grid
            .scrollback
            .iter()
            .chain(&grid.cells)
            .map(|row| row_text(row))
            .collect();
        for line in 0..6 {
            assert!(
                output.contains(&format!("PRIMARY_LOG_{line:02}")),
                "repaint at {width}x25 lost history: {output:?}"
            );
        }
        assert!(output.contains("WRAPabcdefghijklmnopqrstuvwxyz0123456789END"));
        assert!(output.contains("ACK_MOVE_1"));
    }
}

#[test]
fn conpty_resize_consumes_pending_line_feed_before_growing_viewport() {
    let mut initial = String::new();
    for line in 0..6 {
        initial.push_str(&format!("PRIMARY_LOG_{line:02}\r\n"));
    }
    initial.push_str(
        "WRAPabcdefghijklmnopqrstuvwxyz0123456789END\r\nRESIZE_READY\r\nMOVE_0\r\nACK_MOVE_0\r",
    );
    let mut terminal = TerminalEmulator::test_with_output(90, 8, &initial);
    terminal.grid.is_conpty = true;
    assert_eq!(row_text(&terminal.grid.cells[0]), "PRIMARY_LOG_02");
    // ConPTY delivered the acknowledgement before its final line feed. That
    // line feed belongs to the old viewport, where it archives PRIMARY_LOG_02.
    terminal.test_queue_output(b"\n");
    terminal.resize(20, 25);
    terminal.process();
    terminal.test_queue_output(
        b"\x1b[?25l\x1b[HPRIMARY_LOG_03      \r\nPRIMARY_LOG_04      \r\nPRIMARY_LOG_05      \r\nWRAPabcdefghijklmnopqrstuvwxyz0123456789END\x1b[K\r\nRESIZE_READY        \r\nMOVE_0\x1b[K\r\nACK_MOVE_0\x1b[K\r\nMOVE_1\x1b[K\r\nACK_MOVE_1\x1b[K\r\n\x1b[K\x1b[12;1H\x1b[?25h",
    );
    terminal.process();
    let output: String = terminal
        .grid
        .scrollback
        .iter()
        .chain(&terminal.grid.cells)
        .map(|row| row_text(row))
        .collect();
    for line in 0..6 {
        let marker = format!("PRIMARY_LOG_{line:02}");
        assert_eq!(output.matches(&marker).count(), 1, "{output:?}");
    }
    assert!(output.contains("WRAPabcdefghijklmnopqrstuvwxyz0123456789END"));
    assert!(output.contains("ACK_MOVE_1"));
}

#[test]
fn resize_defers_large_output_backlog_without_changing_viewport() {
    let mut terminal = TerminalEmulator::test_with_output(20, 2, "FIRST\r\nSECOND\r");
    terminal.grid.is_conpty = true;
    let observed = terminal.wake.generation();
    // A burst of NULs leaves a final line feed for the next bounded drain.
    terminal.test_queue_output(&vec![0; 64 * 1024]);
    terminal.test_queue_output(b"\n");

    terminal.resize(20, 5);
    assert_eq!(terminal.last_size, (20, 2));
    assert_eq!((terminal.grid.width, terminal.grid.height), (20, 2));
    assert_eq!(*terminal.incoming.lock().unwrap(), b"\n");
    assert_ne!(terminal.wake.generation(), observed);

    for _ in 0..MAX_PTY_BYTES_PER_TICK / PTY_PARSE_QUANTUM + 2 {
        terminal.resize(20, 5);
        if terminal.last_size == (20, 5) { break; }
    }
    assert_eq!(terminal.last_size, (20, 5));
    assert_eq!(row_text(&terminal.grid.scrollback[0]), "FIRST");
    assert_eq!(row_text(&terminal.grid.cells[0]), "SECOND");
    assert!(terminal.incoming.lock().unwrap().is_empty());
}

#[test]
fn conpty_resize_repaint_preserves_rows_displaced_by_coalesced_input() {
    let mut initial = String::new();
    for line in 0..6 {
        initial.push_str(&format!("PRIMARY_LOG_{line:02}\r\n"));
    }
    initial.push_str("WRAPabcdefghijklmnopqrstuvwxyz0123456789END\r\nRESIZE_READY\r\n");
    // Captured Windows 26100 output starts at a different row depending on
    // whether the resize frame includes the input echo, its reply, or neither.
    for first_visible in 1..=3 {
        for chunk_size in [1, 7, usize::MAX] {
            let mut terminal = TerminalEmulator::test_with_output(32, 20, &initial);
            terminal.grid.is_conpty = true;
            terminal.resize(90, 8);
            let mut repaint = String::from("\x1b[?25l\x1b[8;8;90t\x1b[H");
            for line in first_visible..6 {
                repaint.push_str(&format!("PRIMARY_LOG_{line:02}\x1b[K\r\n"));
            }
            repaint.push_str("WRAPabcdefghijklmnopqrstuvwxyz0123456789END\x1b[K\r\nRESIZE_READY\x1b[K\r\n");
            if first_visible >= 2 {
                repaint.push_str("MOVE_0\x1b[K\r\n");
            }
            if first_visible == 3 {
                repaint.push_str("ACK_MOVE_0\x1b[K\r\n");
            }
            repaint.push_str("\x1b[K\x1b[?25h");
            if first_visible == 1 {
                repaint.push_str("MOVE_0\r\n");
            }
            if first_visible <= 2 {
                repaint.push_str("ACK_MOVE_0\r\n");
            }
            for chunk in repaint.as_bytes().chunks(chunk_size) {
                terminal.test_queue_output(chunk);
                terminal.process();
            }
            let output: String = terminal.grid.scrollback.iter().chain(&terminal.grid.cells)
                .map(|row| row_text(row)).collect();
            for line in 0..6 {
                let marker = format!("PRIMARY_LOG_{line:02}");
                assert_eq!(output.matches(&marker).count(), 1, "first row {first_visible}, chunk {chunk_size}: {output:?}");
            }
            assert!(output.contains("WRAPabcdefghijklmnopqrstuvwxyz0123456789END"));
            assert_eq!(output.matches("ACK_MOVE_0").count(), 1);
        }
    }
}

#[test]
fn conpty_resize_repaint_preserves_rows_displaced_while_narrowing_and_growing() {
    let mut initial = String::new();
    for line in 0..6 {
        initial.push_str(&format!("PRIMARY_LOG_{line:02}\r\n"));
    }
    initial.push_str(
        "WRAPabcdefghijklmnopqrstuvwxyz0123456789END\r\nRESIZE_READY\r\nMOVE_0\r\nACK_MOVE_0\r\n",
    );
    for width in [20, 16] {
        let mut terminal = TerminalEmulator::test_with_output(90, 8, &initial);
        terminal.grid.is_conpty = true;
        assert_eq!(row_text(&terminal.grid.cells[0]), "PRIMARY_LOG_03");
        terminal.resize(width, 25);
        // The input echo can arrive before the delayed resize frame. It must
        // not cancel recovery of rows that the following frame omits.
        terminal.test_queue_output(b"MOVE_1\r\n");
        terminal.process();
        terminal.test_queue_output(
            b"\x1b[?25l\x1b[HPRIMARY_LOG_05\x1b[K\r\nWRAPabcdefghijklmnopqrstuvwxyz0123456789END\x1b[K\r\nRESIZE_READY\x1b[K\r\nMOVE_0\x1b[K\r\nACK_MOVE_0\x1b[K\r\nMOVE_1\x1b[K\r\nACK_MOVE_1\x1b[K\r\n\x1b[J\x1b[?25h",
        );
        terminal.process();
        terminal.resize(80, 12);
        let output: String = terminal.grid
            .scrollback
            .iter()
            .chain(&terminal.grid.cells)
            .map(|row| row_text(row))
            .collect();
        for line in 0..6 {
            let marker = format!("PRIMARY_LOG_{line:02}");
            assert_eq!(
                output.matches(&marker).count(),
                1,
                "repaint at {width}x25 lost or duplicated {marker}: {output:?}"
            );
        }
        assert!(output.contains("WRAPabcdefghijklmnopqrstuvwxyz0123456789END"));
        assert!(output.contains("ACK_MOVE_0"));
    }
}

#[test]
fn conpty_resize_recovery_keeps_repeated_rows_and_their_styles() {
    let mut terminal = TerminalEmulator::test_with_output(
        20, 8, "\x1b[31mrepeat\r\nrepeat\r\n\x1b[0munique\r\nlast\r\n",
    );
    terminal.grid.is_conpty = true;
    terminal.resize(30, 6);
    terminal.test_queue_output(
        b"\x1b[?25l\x1b[Hrepeat\x1b[K\r\nunique\x1b[K\r\nlast\x1b[K\r\nnew\x1b[K\r\n\x1b[J\x1b[?25h",
    );
    terminal.process();
    let output: String = terminal.grid.scrollback.iter().chain(&terminal.grid.cells)
        .map(|row| row_text(row)).collect();
    assert_eq!(output, "repeatrepeatuniquelastnew");
    assert_eq!(terminal.grid.scrollback[0][0].fg, Color::Red);
}

#[test]
fn conpty_resize_recovery_does_not_resurrect_a_cleared_screen() {
    let mut terminal = TerminalEmulator::test_with_output(20, 8, "old\r\nkeep\r\nlast\r\n");
    terminal.grid.is_conpty = true;
    terminal.resize(30, 6);
    terminal.test_queue_output(
        b"\x1b[?25l\x1b[2J\x1b[Hkeep\r\nlast\r\n\x1b[?25h",
    );
    terminal.process();
    assert!(terminal.grid.scrollback.is_empty());
    assert_eq!(row_text(&terminal.grid.cells[0]), "keep");
    assert!(terminal.grid.conpty_resize.is_none());
}

#[test]
fn conpty_resize_recovery_preserves_a_displaced_wrapped_line() {
    let mut terminal = TerminalEmulator::test_with_output(
        8, 4, "start\r\n\x1b[31mabcdefghij\x1b[0m\r\nready\r\n",
    );
    terminal.grid.is_conpty = true;
    terminal.resize(4, 8);
    terminal.test_queue_output(
        b"\x1b[?25l\x1b[Hij\x1b[K\r\nready\x1b[K\r\nnext\r\n\x1b[J\x1b[?25h",
    );
    terminal.process();
    assert_eq!(row_text(&terminal.grid.scrollback[2]), "abcd");
    assert_eq!(row_text(&terminal.grid.scrollback[3]), "efgh");
    assert!(terminal.grid.scrollback_wrapped[2]);
    assert!(terminal.grid.scrollback_wrapped[3]);
    assert_eq!(terminal.grid.scrollback[2][0].fg, Color::Red);
    terminal.resize(20, 8);
    let output: String = terminal.grid.scrollback.iter().chain(&terminal.grid.cells)
        .map(|row| row_text(row)).collect();
    assert_eq!(output, "startabcdefghijreadynext");
}

#[test]
fn conpty_resize_recovery_finishes_when_the_host_uses_an_incremental_repaint() {
    let mut terminal = TerminalEmulator::test_with_output(20, 8, "old\r\nkeep\r\nlast\r\n");
    terminal.grid.is_conpty = true;
    terminal.resize(30, 6);
    terminal.test_queue_output(b"\x1b[4;1Hnew\r\n");
    terminal.process();
    assert!(terminal.grid.conpty_resize.is_none());
    assert!(terminal.grid.scrollback.is_empty());
    assert_eq!(row_text(&terminal.grid.cells[3]), "new");
}

#[test]
fn conpty_resize_recovery_does_not_archive_alternate_screen_rows() {
    let mut terminal = TerminalEmulator::test_with_output(20, 8, "old\r\nkeep\r\nlast\r\n");
    terminal.grid.is_conpty = true;
    terminal.resize(30, 6);
    terminal.test_queue_output(
        b"\x1b[?1049h\x1b[?25l\x1b[Hkeep\r\nlast\r\n\x1b[?25h\x1b[?1049l",
    );
    terminal.process();
    assert!(terminal.grid.conpty_resize.is_none());
    assert!(terminal.grid.scrollback.is_empty());
    assert_eq!(row_text(&terminal.grid.cells[0]), "old");
}

#[test]
fn conpty_reflow_keeps_wrapped_history_outside_the_live_viewport() {
    for width in [3, 16] {
        let mut grid = TermGrid::new(4, 2);
        grid.is_conpty = true;
        feed(&mut grid, "abcdefghijkl\r\n");
        assert_eq!(grid.scrollback.len(), 2);

        grid.resize(width, 8);
        feed(&mut grid, "\x1b[2J\x1b[Hijkl\r\nNEXT");
        let output: String = grid
            .scrollback
            .iter()
            .chain(&grid.cells)
            .map(|row| row_text(row))
            .collect();
        assert_eq!(output, "abcdefghijklNEXT", "repaint at {width}x8");
    }
}

#[test]
fn resize_preserves_pending_wrap_and_hard_line_breaks() {
    let mut grid = TermGrid::new(4, 6);
    feed(&mut grid, "abcd");
    grid.resize(8, 6);
    feed(&mut grid, "efgh");
    grid.resize(4, 6);
    feed(&mut grid, "i\r\nseparate");
    grid.resize(20, 6);
    assert_eq!(row_text(grid.display_line(0)), "abcdefghi");
    assert_eq!(row_text(grid.display_line(1)), "separate");
}

#[test]
fn moving_a_scrolled_terminal_keeps_the_same_history_visible() {
    let mut grid = TermGrid::new(30, 5);
    for line in 0..20 {
        feed(&mut grid, &format!("log {line}\r\n"));
    }
    grid.scroll_view = 10;
    let top = row_text(grid.display_line(0));
    for (width, height) in [(40, 3), (20, 8), (30, 5)] {
        grid.resize(width, height);
        assert_eq!(row_text(grid.display_line(0)), top);
    }
}

#[test]
fn simultaneous_reflow_and_height_change_keeps_the_scrolled_log_in_view() {
    let mut grid = TermGrid::new(10, 8);
    feed(&mut grid, &format!("{}\r\n> ", "abcdefghij".repeat(20)));
    grid.scroll_view = 6;
    grid.resize(40, 2);
    assert!(
        grid.scroll_view > 0,
        "resizing must not jump from history to the live prompt"
    );
    assert_eq!(row_text(grid.display_line(0)), "abcdefghij".repeat(4));
}

#[test]
fn resized_alternate_screen_restores_primary_output_at_the_new_size() {
    let mut grid = TermGrid::new(30, 12);
    feed(
        &mut grid,
        "shell log stays here\r\n> \x1b[?1049hfull screen app",
    );
    grid.resize(8, 4);
    feed(&mut grid, "\x1b[?1049l");
    assert_eq!(grid.cells.len(), 4);
    assert!(grid.cells.iter().all(|row| row.len() == 8));
    grid.resize(40, 12);
    assert_eq!(row_text(grid.display_line(0)), "shell log stays here");
    assert_eq!((grid.cursor_row, grid.cursor_col), (1, 2));
    feed(&mut grid, "ok");
    assert_eq!(row_text(grid.display_line(1)), "> ok");
}

#[test]
fn saved_cursor_and_scroll_region_stay_valid_after_resize() {
    let mut grid = TermGrid::new(20, 12);
    feed(
        &mut grid,
        "abcdefghijklmnopqrstuv\x1b7\r\nlater log\x1b[8;12r",
    );
    grid.resize(10, 3);
    assert_eq!((grid.scroll_top, grid.scroll_bottom), (0, 2));
    feed(&mut grid, "\x1b8!");
    grid.resize(40, 12);
    assert_eq!(row_text(grid.display_line(0)), "abcdefghijklmnopqrstuv!");
    assert_eq!(row_text(grid.display_line(1)), "later log");
    // Shrinking to a single cell must not leave a stale saved cursor or wrap
    // position that could index outside the grid on the next input byte.
    grid.resize(1, 1);
    feed(&mut grid, "\x1b8x");
    assert_eq!(grid.cells.len(), 1);
    assert_eq!(grid.cells[0].len(), 1);
}

#[test]
#[ignore = "subprocess fixture for terminal resize tests"]
fn live_resize_child() {
    print!("\x1b[2J\x1b[H");
    for line in 0..6 {
        println!("PRIMARY_LOG_{line:02}\r");
    }
    println!("WRAPabcdefghijklmnopqrstuvwxyz0123456789END\r");
    println!("RESIZE_READY\r");
    std::io::stdout().flush().unwrap();
    loop {
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).unwrap() == 0 {
            break;
        }
        println!("ACK_{}\r", input.trim());
        std::io::stdout().flush().unwrap();
    }
}

#[test]
fn running_pty_keeps_logs_and_accepts_input_after_repeated_resizes() {
    use std::time::{Duration, Instant};
    let program = std::env::current_exe().unwrap();
    let args = [
        "--ignored",
        "--exact",
        "terminal::resize_tests::live_resize_child",
        "--nocapture",
    ]
    .map(String::from);
    let mut terminal = TerminalEmulator::spawn_command_in_dir(
        program.to_str().unwrap(),
        &args,
        32,
        20,
        &std::env::current_dir().unwrap(),
    )
    .unwrap();
    let text = |terminal: &TerminalEmulator| -> String {
        terminal
            .grid
            .scrollback
            .iter()
            .chain(&terminal.grid.cells)
            .map(|row| row_text(row))
            .collect()
    };
    let completed_line = |terminal: &TerminalEmulator, marker: &str| {
        // A marker can arrive before its line feed or the end of ConPTY's
        // repaint. Resize only after the fixture has completed the handshake.
        text(terminal).contains(marker)
            && terminal.grid.cursor_visible
            && terminal.grid.cursor_col == 0
            && row_text(&terminal.grid.cells[terminal.grid.cursor_row]).is_empty()
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !completed_line(&terminal, "RESIZE_READY") && Instant::now() < deadline {
        terminal.process();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(completed_line(&terminal, "RESIZE_READY"));
    for (index, (width, height)) in [(90, 8), (20, 25), (80, 12)].into_iter().enumerate() {
        terminal.resize(width, height);
        terminal.write(format!("MOVE_{index}\r\n").as_bytes());
        let marker = format!("ACK_MOVE_{index}");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !completed_line(&terminal, &marker) && Instant::now() < deadline {
            terminal.process();
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = text(&terminal);
        assert!(
            completed_line(&terminal, &marker),
            "PTY must still accept input: {output:?}"
        );
        for line in 0..6 {
            let marker = format!("PRIMARY_LOG_{line:02}");
            assert_eq!(
                output.matches(&marker).count(),
                1,
                "log lost or duplicated at {width}x{height}: {output:?}"
            );
        }
        assert!(
            output.contains("WRAPabcdefghijklmnopqrstuvwxyz0123456789END"),
            "wrapped log lost: {output:?}"
        );
    }
}

// The pre-optimization reflow is retained only for parity and release profiling.
fn reference_reflow(grid: &mut TermGrid, width: usize, height: usize) {
    let history = grid.scrollback.len();
    // Map the live cursor, saved cursor, and top of the scrolled viewport
    // through the same logical lines as the text, including delayed wraps.
    let mut anchors = vec![(
        history + grid.cursor_row,
        grid.cursor_col,
        grid.pending_wrap,
    )];
    if let Some(saved) = grid.saved_cursor {
        anchors.push((history + saved.0, saved.1, false));
    }
    let view_anchor = if grid.scroll_view > 0 {
        anchors.push((history.saturating_sub(grid.scroll_view), 0, false));
        Some(anchors.len() - 1)
    } else {
        None
    };
    let history_anchor = if grid.is_conpty {
        anchors.push((history, 0, false));
        Some(anchors.len() - 1)
    } else {
        None
    };
    let mut mapped = vec![(0, 0, false); anchors.len()];
    let mut rows: Vec<_> = grid
        .scrollback
        .drain(..)
        .chain(grid.cells.drain(..))
        .collect();
    let wraps: Vec<_> = grid
        .scrollback_wrapped
        .drain(..)
        .chain(grid.wrapped.drain(..))
        .collect();
    let last_anchor = anchors.iter().map(|anchor| anchor.0).max().unwrap_or(0);
    while rows.len() > last_anchor + 1
        && rows
            .last()
            .unwrap()
            .iter()
            .all(|cell| *cell == TermCell::default())
    {
        rows.pop();
    }
    let mut output = Vec::new();
    let mut output_wrapped = Vec::new();
    let mut logical = Vec::new();
    let row_count = rows.len();
    for (index, mut row) in rows.into_iter().enumerate() {
        // Reflow ConPTY's history separately from its viewport, including
        // lines that originally wrapped across that boundary. A repaint
        // must never receive a row containing any historical text.
        let wrapped =
            wraps[index] && index + 1 < row_count && !(grid.is_conpty && index + 1 == history);
        let mut len = if wrapped {
            row.len()
        } else {
            row.iter()
                .rposition(|cell| *cell != TermCell::default())
                .map_or(0, |col| col + 1)
        };
        for (anchor, &(anchor_row, col, pending)) in anchors.iter().enumerate() {
            if anchor_row != index {
                continue;
            }
            let offset = logical.len() + col + usize::from(pending);
            let delayed = pending && offset > 0 && offset % width == 0;
            let cell_offset = offset.saturating_sub(usize::from(delayed));
            mapped[anchor] = (
                output.len() + cell_offset / width,
                cell_offset % width,
                delayed,
            );
            len = len.max(col + 1);
        }
        row.resize(len, TermCell::default());
        logical.extend(row);
        if !wrapped {
            if logical.is_empty() {
                output.push(TermRow::from(vec![TermCell::default(); width]));
                output_wrapped.push(false);
            } else {
                let chunks = logical.len().div_ceil(width);
                for (chunk_index, chunk) in logical.chunks(width).enumerate() {
                    let mut row = chunk.to_vec();
                    row.resize(width, TermCell::default());
                    output.push(row.into());
                    output_wrapped.push(chunk_index + 1 < chunks);
                }
            }
            logical.clear();
        }
    }
    let mut live_start = output.len().saturating_sub(height);
    if let Some(anchor) = history_anchor {
        // Keep the reflowed history outside ConPTY's live viewport even
        // when width and height change together.
        live_start = live_start.max(mapped[anchor].0).min(output.len());
    }
    grid.cells = output.split_off(live_start);
    grid.wrapped = output_wrapped.split_off(live_start);
    grid.scrollback = output.into();
    grid.scrollback_wrapped = output_wrapped.into();
    grid.cells.resize_with(height, || vec![TermCell::default(); width].into());
    grid.wrapped.resize(height, false);
    grid.cursor_row = mapped[0].0.saturating_sub(live_start);
    grid.cursor_col = mapped[0].1;
    grid.pending_wrap = mapped[0].2;
    if let Some(saved) = &mut grid.saved_cursor {
        saved.0 = mapped[1].0.saturating_sub(live_start);
        saved.1 = mapped[1].1;
    }
    if let Some(anchor) = view_anchor {
        grid.scroll_view = live_start.saturating_sub(mapped[anchor].0);
    }
}

fn reference_resize(grid: &mut TermGrid, width: usize, height: usize) {
    reference_reflow(grid, width, height);
    grid.width = width;
    grid.height = height;
    grid.cursor_row = grid.cursor_row.min(height - 1);
    grid.cursor_col = grid.cursor_col.min(width - 1);
    if let Some(saved) = &mut grid.saved_cursor {
        saved.0 = saved.0.min(height - 1);
        saved.1 = saved.1.min(width - 1);
    }
    grid.trim_scrollback();
}

fn resize_log_fixture(lines: usize, conpty: bool) -> TermGrid {
    let mut grid = TermGrid::new(120, 30);
    grid.is_conpty = conpty;
    for line in 0..lines {
        feed(
            &mut grid,
            &format!("\x1b[31;1mlog {line:04}\x1b[0m 界 payload\r\n"),
        );
        if line % 17 == 0 {
            feed(&mut grid, &("long wrapped output ".repeat(8) + "\r\n"));
        }
    }
    feed(&mut grid, "> ");
    grid.scroll_view = 10;
    grid.saved_cursor = Some((
        grid.cursor_row,
        grid.cursor_col,
        Color::Red,
        Color::Reset,
        true,
        false,
        false,
        false,
        false,
        false,
    ));
    grid
}

#[test]
fn terminal_row_reuse_matches_previous_reflow_for_history_wrapping_and_cursor_anchors() {
    for conpty in [false, true] {
        let mut optimized = resize_log_fixture(200, conpty);
        let mut reference = resize_log_fixture(200, conpty);
        for (width, height) in [
            (119, 30),
            (118, 15),
            (80, 20),
            (37, 40),
            (120, 30),
            (1, 30),
            (140, 50),
        ] {
            optimized.resize(width, height);
            reference_resize(&mut reference, width, height);
            assert_eq!(
                optimized.cells, reference.cells,
                "{width}x{height} conpty={conpty}"
            );
            assert_eq!(optimized.scrollback, reference.scrollback);
            assert_eq!(optimized.wrapped, reference.wrapped);
            assert_eq!(optimized.scrollback_wrapped, reference.scrollback_wrapped);
            assert_eq!(optimized.saved_cursor, reference.saved_cursor);
            assert_eq!(
                (
                    optimized.cursor_row,
                    optimized.cursor_col,
                    optimized.pending_wrap,
                    optimized.scroll_view
                ),
                (
                    reference.cursor_row,
                    reference.cursor_col,
                    reference.pending_wrap,
                    reference.scroll_view
                )
            );
        }
    }
}

#[test]
#[ignore = "manual release terminal resize benchmark"]
fn benchmark_terminal_scrollback_width_resizes() {
    use std::{hint::black_box, time::Instant};
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..9 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let mut grid = resize_log_fixture(4_000, false);
            let started = Instant::now();
            for width in [119, 118, 117, 116, 115, 114, 113, 112] {
                if optimized {
                    grid.resize(width, 30);
                } else {
                    reference_resize(&mut grid, width, 30);
                }
                black_box(&grid.cells);
            }
            let elapsed = started.elapsed() / 8;
            if optimized {
                after.push(elapsed);
            } else {
                before.push(elapsed);
            }
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "terminal 4000-line resize rebuild={:?} reuse={:?}",
        before[4], after[4]
    );
}
