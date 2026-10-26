use super::*;

#[test]
fn queued_output_coalesces_wakes_and_rearms_after_draining() {
    let mut terminal = TerminalEmulator::test_with_output(20, 3, "");
    let initial = terminal.wake.generation();
    terminal.test_queue_output(b"");
    assert_eq!(terminal.wake.generation(), initial);
    terminal.test_queue_output(b"a");
    let notified = terminal.wake.generation();
    assert_ne!(notified, initial);
    for _ in 0..50 {
        terminal.test_queue_output(b"b");
    }
    assert_eq!(terminal.wake.generation(), notified);
    assert!(terminal.has_pending_output());
    assert!(terminal.process_while(|| true));
    assert!(!terminal.has_pending_output());
    assert!(!terminal.process());
    terminal.test_queue_output(b"c");
    assert_ne!(terminal.wake.generation(), notified);
    assert!(terminal.process());
}

#[test]
fn yielding_mid_utf8_and_escape_sequence_keeps_old_bytes_before_new_output() {
    let mut bytes = vec![0; PTY_PARSE_QUANTUM - 1];
    bytes.extend_from_slice("é\x1b[38;2;11;22;33m".as_bytes());
    bytes.resize(PTY_PARSE_QUANTUM * 2 - 3, 0);
    bytes.extend_from_slice(b"\x1b[3;4HZ");
    bytes.extend_from_slice("世界\r\n".repeat(100).as_bytes());
    let mut terminal = TerminalEmulator::test_with_output(20, 3, "");
    terminal.test_queue_output(&bytes);
    let wake = terminal.wake.generation();
    assert!(terminal.process_while(|| false));
    assert_eq!(terminal.output_offset, PTY_PARSE_QUANTUM);
    assert_eq!(terminal.grid.cells[0][0].ch, ' ');
    assert!(terminal.incoming.lock().unwrap().is_empty());
    assert!(
        terminal.has_pending_output(),
        "the retained batch is still pending"
    );
    assert_ne!(terminal.wake.generation(), wake);
    let allocation = terminal.output_batch.as_ptr();
    terminal.test_queue_output(b"newer");
    while terminal.process_while(|| false) {}
    assert_eq!(terminal.output_batch.as_ptr(), allocation);

    let mut expected = TermGrid::new(20, 3);
    let mut parser = vte::Parser::new();
    for byte in bytes.iter().chain(b"newer") {
        parser.advance(&mut expected, *byte);
    }
    assert_eq!(terminal.grid.cells, expected.cells);
    assert_eq!(terminal.grid.scrollback, expected.scrollback);
    assert_eq!(terminal.grid.wrapped, expected.wrapped);
    assert_eq!(
        (terminal.grid.cursor_row, terminal.grid.cursor_col),
        (expected.cursor_row, expected.cursor_col)
    );
    assert!(!terminal.has_pending_output());
}

#[test]
fn wrapped_output_batches_keep_order_across_yields_and_reuse_storage() {
    let capacity = MAX_PTY_BYTES_PER_TICK + 10_000;
    for head in [1, capacity / 2, capacity - 1] {
        let mut queue = VecDeque::with_capacity(capacity);
        queue.extend("line café 世界\x1b[32m green\x1b[0m\r\n".bytes().cycle().take(capacity));
        for _ in 0..head {
            let byte = queue.pop_front().unwrap();
            queue.push_back(byte);
        }
        assert!(!queue.as_slices().1.is_empty(), "exercise both ring spans");
        let bytes: Vec<_> = queue.iter().copied().collect();
        let mut terminal = TerminalEmulator::test_with_output(40, 4, "");
        *terminal.incoming.lock().unwrap() = queue;
        terminal.incoming_ready.store(true, Ordering::Release);
        assert!(terminal.process_while(|| false));
        assert_eq!(terminal.output_offset, PTY_PARSE_QUANTUM);
        assert_eq!(terminal.output_batch, bytes[..MAX_PTY_BYTES_PER_TICK]);
        let allocation = terminal.output_batch.as_ptr();
        terminal.test_queue_output(b"\r\nnewer output");
        while terminal.process_while(|| false) {}
        assert!(!terminal.has_pending_output());
        assert!(terminal.incoming.lock().unwrap().is_empty());
        assert_eq!(terminal.output_batch.as_ptr(), allocation);

        let mut expected = TermGrid::new(40, 4);
        let mut parser = vte::Parser::new();
        for &byte in bytes.iter().chain(b"\r\nnewer output") {
            parser.advance(&mut expected, byte);
        }
        assert_eq!(terminal.grid.cells, expected.cells);
        assert_eq!(terminal.grid.scrollback, expected.scrollback);
        assert_eq!(terminal.grid.wrapped, expected.wrapped);
        assert_eq!((terminal.grid.cursor_row, terminal.grid.cursor_col),
            (expected.cursor_row, expected.cursor_col));
    }
}

#[test]
fn expired_budget_makes_bounded_progress_and_resize_waits_for_both_queues() {
    let mut terminal = TerminalEmulator::test_with_output(20, 2, "FIRST\r\nSECOND\r");
    terminal.test_queue_output(&vec![0; MAX_PTY_BYTES_PER_TICK * 2]);
    terminal.test_queue_output(b"\n");
    assert!(terminal.process_until(Instant::now()));
    assert_eq!(terminal.output_offset, PTY_PARSE_QUANTUM);
    terminal.resize(20, 5);
    assert_eq!(terminal.last_size, (20, 2));
    assert!(terminal.has_pending_output());
    while terminal.process_while(|| true) {}
    terminal.resize(20, 5);
    assert_eq!(terminal.last_size, (20, 5));
    let output: String = terminal
        .grid
        .scrollback
        .iter()
        .chain(&terminal.grid.cells)
        .flat_map(|row| row.iter().map(|cell| cell.ch))
        .collect();
    assert!(output.contains("FIRST") && output.contains("SECOND"));
}

#[test]
fn concurrent_publish_and_drain_preserve_readiness_and_bytes() {
    let mut terminal = TerminalEmulator::test_with_output(40, 4, "");
    let incoming = terminal.incoming.clone();
    let ready = terminal.incoming_ready.clone();
    let wake = terminal.wake.clone();
    let source = "line 世界\r\n".repeat(1000);
    let bytes = source.clone();
    let producer = std::thread::spawn(move || {
        for chunk in bytes.as_bytes().chunks(7) {
            publish_output(&incoming, &ready, &wake, chunk);
            std::thread::yield_now();
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !producer.is_finished() || terminal.has_pending_output() {
        assert!(Instant::now() < deadline);
        terminal.process_while(|| false);
        std::thread::yield_now();
    }
    producer.join().unwrap();
    assert!(!terminal.has_pending_output());
    assert!(terminal.incoming.lock().unwrap().is_empty());
    let expected = TerminalEmulator::test_with_output(40, 4, &source);
    assert_eq!(terminal.grid.cells, expected.grid.cells);
    assert_eq!(terminal.grid.scrollback, expected.grid.scrollback);
}

#[test]
#[ignore = "manual comparison of terminal UI-turn latency, throughput, and idle polling"]
fn benchmark_terminal_responsiveness() {
    use std::hint::black_box;

    let bytes = b"\x1b[2J\x1b[Htext\x1b[100S".repeat(4000);
    let mut times = [Vec::new(), Vec::new()];
    for round in 0..21 {
        for index in [round % 2, (round + 1) % 2] {
            let mut terminal = TerminalEmulator::test_with_output(160, 50, "");
            terminal.test_queue_output(&bytes);
            let started = Instant::now();
            if index == 1 {
                black_box(&mut terminal).process();
            } else {
                black_box(&mut terminal).process_while(|| true);
            }
            times[index].push(started.elapsed());
            black_box(&terminal.grid);
        }
    }
    for (index, times) in times.iter_mut().enumerate() {
        times.sort_unstable();
        eprintln!(
            "terminal escape-heavy turn bounded={}: {:?}",
            index == 1,
            times[times.len() / 2]
        );
    }

    // Drain the same complete input on both paths; time-to-yield alone cannot
    // show the clock-check overhead on ordinary, inexpensive output.
    let mut ordinary =
        b"Compiling example: src/main.rs:42: normal output from a running command\r\n".repeat(1024);
    ordinary.truncate(MAX_PTY_BYTES_PER_TICK);
    let mut times = [Vec::new(), Vec::new()];
    let mut turns = [Vec::new(), Vec::new()];
    for round in 0..21 {
        let mut grids = [None, None];
        for index in [round % 2, (round + 1) % 2] {
            let mut terminal = TerminalEmulator::test_with_output(160, 50, "");
            terminal.test_queue_output(&ordinary);
            let started = Instant::now();
            let mut count = 0;
            while terminal.has_pending_output() {
                if index == 1 {
                    black_box(&mut terminal).process();
                } else {
                    black_box(&mut terminal).process_while(|| true);
                }
                count += 1;
            }
            times[index].push(started.elapsed());
            turns[index].push(count);
            grids[index] = Some((
                terminal.grid.cells.clone(),
                terminal.grid.scrollback.clone(),
                terminal.grid.wrapped.clone(),
                (terminal.grid.cursor_row, terminal.grid.cursor_col),
            ));
        }
        let [Some(reference), Some(bounded)] = grids else {
            unreachable!();
        };
        assert_eq!(reference, bounded);
    }
    for (index, times) in times.iter_mut().enumerate() {
        times.sort_unstable();
        turns[index].sort_unstable();
        eprintln!(
            "terminal ordinary {}-byte drain bounded={}: {:?}, median turns={}",
            ordinary.len(),
            index == 1,
            times[times.len() / 2],
            turns[index][turns[index].len() / 2],
        );
    }

    let mut terminal = TerminalEmulator::test_with_output(80, 24, "");
    for reference in [true, false] {
        let start = Instant::now();
        for _ in 0..500_000 {
            let result = if reference {
                !black_box(&terminal).incoming.lock().unwrap().is_empty()
            } else {
                black_box(&mut terminal).process()
            };
            black_box(result);
        }
        eprintln!(
            "idle terminal poll reference={reference}: {:.2} ns",
            start.elapsed().as_secs_f64() * 1e9 / 500_000.0
        );
    }
}
