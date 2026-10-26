use super::*;
use crate::terminal::{TermCell, TermGrid, TermRow, TerminalEmulator};
use crate::theme_workbench::WorkbenchColors;
use ratatui::{backend::Backend, buffer::Buffer, layout::Position};

fn feed(pty: &mut TerminalEmulator, bytes: &[u8]) {
    pty.test_queue_output(bytes);
    while pty.process_while(|| true) {}
}

fn render(
    pty: &TerminalEmulator,
    palette: &WorkbenchColors,
    area: Rect,
    blink: bool,
    reference: bool,
) -> (Buffer, Position) {
    let mut terminal = Terminal::new(TestBackend::new(24, 10)).unwrap();
    terminal
        .draw(|frame| {
            for cell in &mut frame.buffer_mut().content {
                cell.set_char('~').set_style(
                    Style::default()
                        .fg(Color::Yellow)
                        .bg(Color::Blue)
                        .underline_color(Color::Cyan)
                        .add_modifier(Modifier::ITALIC),
                );
            }
            if reference {
                super::reference::reference_terminal_draw(frame, pty, area, palette, blink);
            } else {
                draw_terminal_emulator(frame, pty, area, palette, blink);
            }
        })
        .unwrap();
    let cursor = terminal.backend_mut().get_cursor_position().unwrap();
    (terminal.backend().buffer().clone(), cursor)
}

fn compare(pty: &TerminalEmulator, palette: &WorkbenchColors, area: Rect, blink: bool) {
    assert_eq!(
        render(pty, palette, area, blink, false),
        render(pty, palette, area, blink, true)
    );
}

fn conversions(pty: &TerminalEmulator) -> usize {
    pty.render_rows.borrow().test_stats().0
}

#[test]
fn terminal_idle_cursor_and_selection_changes_reuse_all_base_rows() {
    let mut palette = App::test_new().colors.ui;
    let mut pty = TerminalEmulator::test_with_output(16, 6, "one\r\ntwo\r\n\x1b[31;2mthree\x1b[0m");
    let area = Rect::new(2, 1, 16, 6);
    compare(&pty, &palette, area, true);
    assert_eq!(conversions(&pty), 6);
    for shape in 0..=6 {
        pty.grid.cursor_shape = shape;
        pty.grid.cursor_col = (shape as usize * 2) % 16;
        for blink in [false, true] {
            compare(&pty, &palette, area, blink);
        }
    }
    for end in [(0, 0), (2, 6), (5, 15)] {
        pty.grid.begin_selection(1, 2);
        pty.grid.extend_selection(end.0, end.1);
        pty.grid.finish_selection();
        compare(&pty, &palette, area, true);
        pty.grid.clear_selection();
        compare(&pty, &palette, area, true);
    }
    palette.bg_terminal_selection = Color::Rgb(20, 30, 80);
    palette.fg_terminal_selection = Some(Color::White);
    palette.bg_terminal_cursor = Color::Rgb(100, 90, 80);
    palette.fg_terminal_cursor = Color::Black;
    pty.grid.begin_selection(0, 0);
    pty.grid.extend_selection(2, 8);
    compare(&pty, &palette, area, true);
    pty.grid.clear_selection();
    compare(&pty, &palette, area, true);
    assert_eq!(
        conversions(&pty),
        6,
        "overlays must never invalidate base text"
    );
}

#[test]
fn terminal_row_mutations_clone_and_movement_have_safe_damage_identities() {
    let mut grid = TermGrid::new(8, 4);
    let keys: Vec<_> = grid
        .cells
        .iter()
        .map(|row| (row.identity(), row.generation()))
        .collect();
    grid.cells[1][2].ch = '界';
    assert!(grid.cells[1].generation() > keys[1].1);
    for index in [0, 2, 3] {
        assert_eq!(
            (grid.cells[index].identity(), grid.cells[index].generation()),
            keys[index]
        );
    }
    let generation = grid.cells[1].generation();
    for cell in &mut grid.cells[1] {
        cell.bold = true;
    }
    assert!(grid.cells[1].generation() > generation);
    let mut copy = grid.cells[1].clone();
    assert_eq!(copy, grid.cells[1]);
    assert_ne!(copy.identity(), grid.cells[1].identity());
    copy[0].ch = 'X';
    grid.cells[1][0].ch = 'Y';
    assert_ne!(copy, grid.cells[1]);
    let keys: Vec<_> = grid
        .cells
        .iter()
        .map(|row| (row.identity(), row.generation()))
        .collect();
    grid.cells.rotate_left(1);
    for index in 0..4 {
        assert_eq!(
            (grid.cells[index].identity(), grid.cells[index].generation()),
            keys[(index + 1) % 4]
        );
    }
    let mut empty = TermRow::default();
    empty.push(TermCell::default());
    assert_ne!(empty.identity(), 0);
}

#[test]
fn terminal_single_row_output_and_cell_operations_only_reconvert_that_row() {
    let palette = App::test_new().colors.ui;
    let mut pty = TerminalEmulator::test_with_output(16, 6, "hello\r\nworld");
    let area = Rect::new(0, 0, 16, 6);
    compare(&pty, &palette, area, true);
    let before = conversions(&pty);
    feed(&mut pty, b"\x1b[4;3H\x1b[1;3;4;31m\x1b[?25l");
    compare(&pty, &palette, area, true);
    assert_eq!(
        conversions(&pty),
        before,
        "cursor and attribute state alone do not alter rows"
    );
    let identity = pty.grid.cells[3].identity();
    let allocation = pty.render_rows.borrow().test_row(identity).unwrap().0;
    for command in [
        "界é👩‍💻",
        "\x1b[2P",
        "\x1b[2@",
        "\x1b[3X",
        "\x1b[1K",
        "\x1b[2K",
    ] {
        let before = conversions(&pty);
        feed(&mut pty, command.as_bytes());
        compare(&pty, &palette, area, true);
        assert_eq!(conversions(&pty), before + 1, "{command:?}");
        assert_eq!(
            pty.render_rows.borrow().test_row(identity).unwrap().0,
            allocation
        );
    }
    let before = conversions(&pty);
    pty.grid.cells[0][0].fg = Color::Indexed(234);
    pty.grid.cells[0][0].ch = '\0';
    pty.grid.cells[5].fill(TermCell {
        ch: '!',
        bold: true,
        ..TermCell::default()
    });
    compare(&pty, &palette, area, true);
    assert_eq!(conversions(&pty), before + 2);
}

#[test]
fn terminal_scroll_damage_reuses_moved_rows_and_bounds_converted_storage() {
    let palette = App::test_new().colors.ui;
    let mut pty = TerminalEmulator::test_with_output(12, 4, "one\r\ntwo\r\nthree\r\nfour");
    let area = Rect::new(0, 0, 12, 4);
    compare(&pty, &palette, area, true);
    let before = conversions(&pty);
    feed(&mut pty, b"\r\n");
    compare(&pty, &palette, area, true);
    assert_eq!(conversions(&pty), before + 1);
    let before = conversions(&pty);
    pty.scroll_up(1);
    compare(&pty, &palette, area, true);
    assert_eq!(
        conversions(&pty),
        before,
        "scrolling to a cached history row only moves its pixels"
    );
    pty.scroll_down(1);
    let mut buffers = std::collections::HashSet::new();
    let mut target = Buffer::empty(area);
    for step in 0..5_100 {
        feed(&mut pty, b"line\r\n");
        pty.render_rows
            .borrow_mut()
            .paint(&mut target, &pty.grid, area, &palette);
        let cache = pty.render_rows.borrow();
        assert!(cache.test_stats().1 <= 8);
        for row in &pty.grid.cells {
            buffers.insert(cache.test_row(row.identity()).unwrap().0);
        }
        if step % 500 == 0 {
            drop(cache);
            compare(&pty, &palette, area, false);
        }
    }
    assert!(
        buffers.len() <= 8,
        "evicted conversion storage should be recycled, not reallocated"
    );
    assert_eq!(pty.grid.scrollback.len(), 5_000);
}

#[test]
fn terminal_recycled_rows_do_not_share_validity_at_equal_generations() {
    let palette = App::test_new().colors.ui;
    let mut pty = TerminalEmulator::test_with_output(12, 4, "");
    let area = Rect::new(0, 0, 12, 4);
    compare(&pty, &palette, area, true);
    for round in 0..30 {
        for (index, row) in pty.grid.cells.iter_mut().enumerate() {
            *row = vec![
                TermCell {
                    ch: char::from(b'a' + (round + index) as u8 % 26),
                    fg: Color::Indexed((round + index) as u8),
                    ..TermCell::default()
                };
                12
            ]
            .into();
            assert_eq!(row.generation(), 0);
        }
        let before = conversions(&pty);
        compare(&pty, &palette, area, true);
        assert_eq!(conversions(&pty), before + 4);
        let before = conversions(&pty);
        compare(&pty, &palette, area, false);
        assert_eq!(conversions(&pty), before);
    }
}

#[test]
fn terminal_conversion_budget_counts_capacities_across_scrolling_and_resizing() {
    let palette = App::test_new().colors.ui;
    let mut pty = TerminalEmulator::test_with_output(4_096, 40, "");
    let area = Rect::new(0, 0, 4_096, 40);
    let mut buffer = Buffer::empty(area);
    for step in 0..80 {
        feed(&mut pty, b"line\r\n");
        let width = if step > 60 { 16 } else { 4_096 };
        pty.render_rows.borrow_mut().paint(
            &mut buffer,
            &pty.grid,
            Rect::new(0, 0, width, 40),
            &palette,
        );
        assert!(pty.render_rows.borrow().test_stats().2 <= 256 * 1024);
    }
    pty.render_rows
        .borrow_mut()
        .paint(&mut buffer, &pty.grid, area, &palette);
    assert!(pty.render_rows.borrow().test_stats().2 <= 256 * 1024);
}

#[test]
fn terminal_cached_rows_follow_palette_geometry_reset_and_screen_swaps() {
    let mut palette = App::test_new().colors.ui;
    let mut pty = TerminalEmulator::test_with_output(16, 6, "\x1b[31mred\x1b[0m\r\nnormal\r\n界é");
    for command in [
        "",
        "\x1b[?1049hALT",
        "\x1b[?1049l",
        "\x1b[2;5r\x1b[3;1H\x1b[2L",
        "\x1b[2M",
        "\x1b[2T",
        "\x1b[2S",
        "\x1b[3J",
        "\x1bcreset",
    ] {
        feed(&mut pty, command.as_bytes());
        for area in [
            Rect::new(0, 0, 16, 6),
            Rect::new(2, 1, 8, 3),
            Rect::new(3, 2, 20, 12),
            Rect::new(0, 0, 0, 10),
            Rect::new(20, 8, 10, 5),
            Rect::new(25, 12, 2, 2),
        ] {
            compare(&pty, &palette, area, true);
        }
    }
    for (width, height) in [(8, 3), (20, 8), (16, 6)] {
        pty.resize(width, height);
        compare(&pty, &palette, Rect::new(0, 0, 20, 8), true);
    }
    for change in 0..3 {
        let before = conversions(&pty);
        match change {
            0 => palette.ansi[1] = Color::Rgb(30, 80, 200),
            1 => palette.fg_terminal = Color::Rgb(80, 110, 130),
            _ => palette.bg_terminal = Color::Rgb(230, 230, 220),
        }
        compare(&pty, &palette, Rect::new(0, 0, 20, 8), true);
        assert!(conversions(&pty) > before);
    }
    pty.grid = TermGrid::new(16, 6);
    compare(&pty, &palette, Rect::new(0, 0, 16, 6), true);
}

#[test]
fn randomized_terminal_output_scroll_selection_and_resize_match_uncached_drawing() {
    let mut palette = App::test_new().colors.ui;
    let mut pty = TerminalEmulator::test_with_output(16, 6, "");
    let commands = [
        "a界é\r\n",
        "\x1b[2J",
        "\x1b[0K",
        "\x1b[1K",
        "\x1b[2K",
        "\x1b[3P",
        "\x1b[2@",
        "\x1b[3X",
        "\x1b[2L",
        "\x1b[1M",
        "\x1b[2S",
        "\x1b[2T",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[0m",
        "\x1bc",
        "\x1b[1;2;3;4;9;38;2;44;88;132;48;5;200mX",
        "\x1b[2;5r",
        "\x1b[?25l",
        "\x1b[?25h",
        "\x1b[3J",
    ];
    let mut seed = 1289u64;
    for step in 0..800 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let choice = (seed >> 32) as usize;
        match choice % 7 {
            0 => feed(
                &mut pty,
                format!("\x1b[{};{}H", choice % 8 + 1, choice % 20 + 1).as_bytes(),
            ),
            1 => pty.scroll_up(choice % 10),
            2 => pty.scroll_down(choice % 10),
            3 => {
                pty.grid
                    .begin_selection(choice % pty.grid.height, choice % pty.grid.width);
                pty.grid.extend_selection(
                    (choice / 7) % pty.grid.height,
                    (choice / 11) % pty.grid.width,
                );
                pty.grid.finish_selection();
            }
            4 => pty.resize([8, 16, 20][choice % 3], [3, 6, 8][choice % 3]),
            _ => feed(&mut pty, commands[choice % commands.len()].as_bytes()),
        }
        if step % 31 == 0 {
            palette.ansi[step % 16] = Color::Indexed(step as u8);
        }
        let area = if step % 3 == 0 {
            Rect::new(3, 2, 20, 8)
        } else {
            Rect::new(0, 0, 16, 6)
        };
        compare(&pty, &palette, area, step % 2 == 0);
        compare(&pty, &palette, area, step % 2 != 0);
    }
}

// The base-row loop from 81239e8, excluding its independent cursor overlay.
// This reference measures the exact previous per-row selection preparation.
fn original_rows(buffer: &mut Buffer, grid: &TermGrid, area: Rect, palette: &WorkbenchColors) {
    let mut selection = grid
        .has_selection()
        .then(|| crate::ui_colors::TerminalSelectionStyle::new(palette));
    for row in 0..area.height as usize {
        let source = grid.display_line(row);
        let selected = grid.selection_columns(row);
        for column in 0..area.width as usize {
            let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y + row as u16)) else {
                continue;
            };
            if let Some(source) = source.get(column) {
                let mut style = Style::default()
                    .fg(palette.terminal_color(source.fg, true))
                    .bg(palette.terminal_color(source.bg, false));
                for (enabled, modifier) in [
                    (source.bold, Modifier::BOLD),
                    (source.underline, Modifier::UNDERLINED),
                    (source.dim, Modifier::DIM),
                    (source.italic, Modifier::ITALIC),
                    (source.strikethrough, Modifier::CROSSED_OUT),
                ] {
                    if enabled {
                        style = style.add_modifier(modifier);
                    }
                }
                if selected.contains(&column)
                    && let Some(selection) = &mut selection
                {
                    style = selection.apply(style, palette.fg_terminal);
                }
                cell.set_char(if source.ch == '\0' { ' ' } else { source.ch })
                    .set_style(style);
            } else {
                cell.set_char(' ').set_style(
                    Style::default()
                        .fg(palette.fg_terminal)
                        .bg(palette.bg_terminal),
                );
            }
        }
    }
}

#[test]
#[ignore = "manual optimized-profile terminal row conversion benchmark"]
fn benchmark_terminal_damage_rows() {
    use std::hint::black_box;
    let palette = App::test_new().colors.ui;
    let area = Rect::new(0, 0, 160, 48);
    for (name, damaged, selected) in [
        ("idle", 0, false),
        ("selection", 0, true),
        ("one changed row", 1, false),
        ("full repaint", 48, false),
    ] {
        let mut pty = TerminalEmulator::test_with_output(160, 48, "");
        for (row_index, row) in pty.grid.cells.iter_mut().enumerate() {
            for (column, cell) in row.iter_mut().enumerate() {
                cell.ch = char::from(b'a' + ((row_index + column) % 26) as u8);
                cell.fg = Color::Indexed((column % 256) as u8);
                cell.bold = row_index % 2 == 0;
                cell.italic = column % 3 == 0;
            }
        }
        if selected {
            pty.grid.begin_selection(2, 4);
            pty.grid.extend_selection(44, 100);
            pty.grid.finish_selection();
        }
        let mut buffer = Buffer::empty(area);
        pty.render_rows
            .borrow_mut()
            .paint(&mut buffer, &pty.grid, area, &palette);
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..11 {
            for cached in [round % 2 == 0, round % 2 != 0] {
                let start = Instant::now();
                for step in 0..100 {
                    for row in pty.grid.cells.iter_mut().take(damaged) {
                        row[0].ch = if step % 2 == 0 { 'x' } else { 'y' };
                    }
                    buffer.reset();
                    if cached {
                        pty.render_rows
                            .borrow_mut()
                            .paint(&mut buffer, &pty.grid, area, &palette);
                    } else {
                        original_rows(&mut buffer, &pty.grid, area, &palette);
                    }
                    black_box(&buffer);
                }
                let elapsed = start.elapsed() / 100;
                if round >= 2 {
                    samples[usize::from(cached)].push(elapsed);
                }
            }
        }
        let mut expected = Buffer::empty(area);
        original_rows(&mut expected, &pty.grid, area, &palette);
        assert_eq!(buffer, expected, "{name}");
        for sample in &mut samples {
            sample.sort_unstable();
        }
        eprintln!(
            "terminal base rows {name}: rebuilt {:?}, damage {:?}",
            samples[0][4], samples[1][4]
        );
    }
}
