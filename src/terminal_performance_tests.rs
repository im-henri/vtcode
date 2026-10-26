use super::*;
use std::time::Instant;

#[test]
fn pending_output_keeps_order_across_ring_wrap_and_overflow() {
    let mut queue = VecDeque::with_capacity(8);
    queue_output(&mut queue, b"abcdef");
    assert_eq!(queue.drain(..5).collect::<Vec<_>>(), b"abcde");
    queue_output(&mut queue, b"ghijkl");
    assert!(!queue.as_slices().1.is_empty(), "exercise wrapped storage");
    assert_eq!(queue.into_iter().collect::<Vec<_>>(), b"fghijkl");

    let mut queue = VecDeque::from(vec![1; MAX_PTY_PENDING_BYTES]);
    queue_output(&mut queue, &[2; 4096]);
    assert_eq!(queue.len(), MAX_PTY_PENDING_BYTES);
    assert!(
        queue
            .iter()
            .take(MAX_PTY_PENDING_BYTES - 4096)
            .all(|&b| b == 1)
    );
    assert!(
        queue
            .iter()
            .skip(MAX_PTY_PENDING_BYTES - 4096)
            .all(|&b| b == 2)
    );
    let oversized: Vec<_> = (0..MAX_PTY_PENDING_BYTES + 17)
        .map(|index| (index % 251) as u8)
        .collect();
    queue_output(&mut queue, &oversized);
    assert!(queue.iter().copied().eq(oversized[17..].iter().copied()));
}

#[test]
fn bounded_output_batches_preserve_split_utf8_and_escape_sequences() {
    let mut terminal = TerminalEmulator::test_with_output(20, 3, "");
    terminal.test_queue_output(&vec![0; MAX_PTY_BYTES_PER_TICK - 1]);
    terminal.test_queue_output("é\x1b[38;2;11;22;33mZ".as_bytes());
    assert!(terminal.process_while(|| true));
    assert_eq!(terminal.output_batch.len(), MAX_PTY_BYTES_PER_TICK);
    let allocation = terminal.output_batch.as_ptr();
    assert_eq!(terminal.grid.cells[0][0].ch, ' ');
    assert!(terminal.process_while(|| true));
    assert_eq!(terminal.output_batch.as_ptr(), allocation);
    assert_eq!(terminal.grid.cells[0][0].ch, 'é');
    assert_eq!(terminal.grid.cells[0][1].ch, 'Z');
    assert_eq!(terminal.grid.cells[0][1].fg, Color::Rgb(11, 22, 33));
    terminal.test_queue_output(&vec![0; MAX_PTY_BYTES_PER_TICK - 3]);
    terminal.test_queue_output(b"\x1b[3;4HZ");
    assert!(terminal.process_while(|| true));
    assert!(terminal.process_while(|| true));
    assert_eq!(terminal.grid.cells[2][3].ch, 'Z');
    assert!(!terminal.process());
}

#[test]
fn bulk_terminal_cell_edits_preserve_prefix_suffix_and_attributes() {
    for action in ['P', '@', 'X'] {
        for column in 0..8 {
            for count in [0usize, 1, 3, 8, 65535] {
                let mut terminal = TerminalEmulator::test_with_output(8, 2, "\x1b[31mabcdefgh");
                let original = terminal.grid.cells[0].clone();
                let n = count.max(1).min(8 - column);
                let blank = TermCell::default();
                let expected: Vec<_> = (0..8)
                    .map(|index| {
                        if index < column {
                            original[index].clone()
                        } else if action == 'P' {
                            original
                                .get(index + n)
                                .cloned()
                                .unwrap_or_else(|| blank.clone())
                        } else if action == '@' {
                            if index < column + n {
                                blank.clone()
                            } else {
                                original[index - n].clone()
                            }
                        } else if index < column + n {
                            blank.clone()
                        } else {
                            original[index].clone()
                        }
                    })
                    .collect();
                let command = format!("\x1b[1;{}H\x1b[{count}{action}", column + 1);
                for byte in command.bytes() {
                    terminal.parser.advance(&mut terminal.grid, byte);
                }
                assert_eq!(terminal.grid.cells[0], expected, "{command:?}");
                assert_eq!(terminal.grid.cursor_col, column);
            }
        }
    }
}

#[test]
fn csi_inline_parameters_keep_extended_colors_and_multiple_modes() {
    let terminal = TerminalEmulator::test_with_output(
        10,
        2,
        "\x1b[?1;7;25;1004;1006;2004h\x1b[1;3;4;38;2;10;20;30;48;5;42mX\x1b[0mY",
    );
    let grid = &terminal.grid;
    assert!(grid.application_cursor_keys && grid.auto_wrap && grid.cursor_visible);
    assert!(grid.focus_reporting && grid.sgr_mouse && grid.bracketed_paste);
    assert_eq!(grid.cells[0][0].fg, Color::Rgb(10, 20, 30));
    assert_eq!(grid.cells[0][0].bg, Color::Indexed(42));
    assert!(grid.cells[0][0].bold && grid.cells[0][0].italic && grid.cells[0][0].underline);
    assert_eq!(grid.cells[0][1].fg, Color::Reset);
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_terminal_backlog_tick() {
    let mut terminal = TerminalEmulator::test_with_output(120, 30, "");
    let bytes = vec![0; 8 * 1024 * 1024];
    let mut samples = Vec::new();
    for run in 0..25 {
        {
            let mut incoming = terminal.incoming.lock().unwrap();
            incoming.clear();
        }
        terminal.test_queue_output(&bytes);
        let started = Instant::now();
        assert!(terminal.process_while(|| true));
        if run >= 4 {
            samples.push(started.elapsed());
        }
        assert_eq!(
            terminal.incoming.lock().unwrap().len(),
            bytes.len() - 64 * 1024
        );
    }
    samples.sort();
    eprintln!(
        "terminal_backlog_tick backlog=8MiB median={:?}",
        samples[samples.len() / 2]
    );
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_terminal_csi_edits() {
    let mut grid = TermGrid::new(160, 40);
    let mut parser = vte::Parser::new();
    let bytes =
        "\x1b[2J\x1b[1;1H\x1b[38;2;11;22;33mhello\x1b[2048P\x1b[2048@\x1b[2048X".repeat(100);
    let mut samples = Vec::new();
    for run in 0..15 {
        let started = Instant::now();
        for byte in bytes.bytes() {
            parser.advance(&mut grid, byte);
        }
        if run >= 4 {
            samples.push(started.elapsed());
        }
        std::hint::black_box(&grid);
    }
    samples.sort();
    eprintln!(
        "terminal_csi_edits bursts=100 median={:?}",
        samples[samples.len() / 2]
    );
}
