use super::*;
use ratatui::{Terminal, backend::TestBackend};
use std::{
    hash::{Hash, Hasher},
    time::Instant,
};

#[path = "ui_terminal_draw_reference.rs"]
mod reference;

#[path = "ui_terminal_damage_tests.rs"]
mod damage_tests;

#[test]
fn terminal_rendering_matches_reference_for_selection_cursors_styles_and_clipping() {
    let mut app = App::test_new();
    for alternate_palette in [false, true] {
        if alternate_palette {
            app.colors.ui.fg_terminal = Color::Rgb(1, 2, 3);
            app.colors.ui.bg_terminal = Color::Rgb(240, 230, 220);
            app.colors.ui.bg_terminal_selection = Color::Rgb(210, 200, 190);
            app.colors.ui.fg_terminal_selection = Some(Color::Blue);
        }
        for selected in [false, true] {
            for shape in 0..=6 {
                for blink in [false, true] {
                    for area in [
                        Rect::new(0, 0, 12, 5),
                        Rect::new(2, 1, 7, 3),
                        Rect::new(3, 2, 20, 8),
                    ] {
                        let mut pty =
                            crate::terminal::TerminalEmulator::test_with_output(12, 5, "");
                        for (index, row) in pty.grid.cells.iter_mut().enumerate() {
                            for (col, cell) in row.iter_mut().enumerate() {
                                cell.ch = ['a', 'é', '界', '\0', ' ', '😀'][col % 6];
                                cell.fg = [
                                    Color::Reset,
                                    Color::Red,
                                    Color::Indexed(13),
                                    Color::Indexed(42),
                                    Color::Rgb(20, 30, 40),
                                ][index];
                                cell.bg = if col % 3 == 0 {
                                    Color::Indexed(6)
                                } else {
                                    Color::Reset
                                };
                                cell.bold = col % 2 == 0;
                                cell.dim = col % 3 == 0;
                                cell.italic = col % 4 == 0;
                                cell.underline = col % 5 == 0;
                                cell.strikethrough = col % 6 == 0;
                            }
                        }
                        pty.grid.cursor_shape = shape;
                        pty.grid.cursor_row = 2;
                        pty.grid.cursor_col = 4;
                        if selected {
                            pty.grid.begin_selection(4, 9);
                            pty.grid.extend_selection(1, 3);
                            pty.grid.finish_selection();
                        }
                        let render = |reference: bool| {
                            let mut terminal = Terminal::new(TestBackend::new(16, 7)).unwrap();
                            terminal
                                .draw(|frame| {
                                    for cell in &mut frame.buffer_mut().content {
                                        cell.set_char('~').set_style(
                                            Style::default()
                                                .fg(Color::Yellow)
                                                .bg(Color::Blue)
                                                .add_modifier(Modifier::ITALIC),
                                        );
                                    }
                                    if reference {
                                        self::reference::reference_terminal_draw(
                                            frame,
                                            &pty,
                                            area,
                                            &app.colors.ui,
                                            blink,
                                        );
                                    } else {
                                        draw_terminal_emulator(
                                            frame,
                                            &pty,
                                            area,
                                            &app.colors.ui,
                                            blink,
                                        );
                                    }
                                })
                                .unwrap();
                            terminal.backend().buffer().clone()
                        };
                        assert_eq!(
                            render(false),
                            render(true),
                            "selected={selected}, shape={shape}, blink={blink}, area={area:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_terminal_redraw() {
    let app = App::test_new();
    for selected in [false, true] {
        let mut pty = crate::terminal::TerminalEmulator::test_with_output(160, 48, "");
        for (row_index, row) in pty.grid.cells.iter_mut().enumerate() {
            for (column, cell) in row.iter_mut().enumerate() {
                cell.ch = char::from(b'a' + ((row_index + column) % 26) as u8);
                cell.fg = Color::Indexed(((column / 20) % 16) as u8);
                cell.bold = row_index % 2 == 0;
            }
        }
        pty.grid.cursor_visible = false;
        if selected {
            pty.grid.begin_selection(2, 4);
            pty.grid.extend_selection(44, 100);
            pty.grid.finish_selection();
        }
        let mut terminal = Terminal::new(TestBackend::new(160, 48)).unwrap();
        let mut samples = [Vec::new(), Vec::new()];
        for run in 0..9 {
            for reference in if run % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let draw = if reference {
                    self::reference::reference_terminal_draw
                } else {
                    draw_terminal_emulator
                };
                let start = Instant::now();
                for _ in 0..100 {
                    terminal
                        .draw(|frame| {
                            draw(frame, &pty, Rect::new(0, 0, 160, 48), &app.colors.ui, true)
                        })
                        .unwrap();
                }
                if run >= 2 {
                    samples[usize::from(!reference)].push(start.elapsed() / 100);
                }
            }
        }
        for sample in &mut samples {
            sample.sort();
        }
        let mut hash = rustc_hash::FxHasher::default();
        for cell in &terminal.backend().buffer().content {
            cell.symbol().hash(&mut hash);
            cell.style().hash(&mut hash);
        }
        eprintln!(
            "terminal_redraw selected={selected} before={:?} after={:?} hash={:016x}",
            samples[0][3],
            samples[1][3],
            hash.finish()
        );
    }
}
