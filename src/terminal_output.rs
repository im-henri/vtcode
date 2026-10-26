//! Batch Crossterm's tiny cursor/style/text writes before reaching stdout.
//! Ratatui flushes after each completed frame, and `execute!` flushes commands,
//! so buffering adds no timer or input latency.

use std::io::{BufWriter, Write};
use ratatui::{Terminal, TerminalOptions, Viewport, backend::Backend, layout::{Rect, Size}};

/// Resize packets already contain the terminal dimensions. Polling the console
/// host from every draw can take a whole scheduling quantum on a busy Windows
/// machine; use those packets and reconcile once when focus returns instead.
pub(crate) fn event_driven_terminal<B: Backend>(backend: B) -> Result<Terminal<B>, B::Error> {
    let size = backend.size()?;
    Terminal::with_options(backend, TerminalOptions {
        viewport: Viewport::Fixed(Rect::new(0, 0, size.width, size.height)),
    })
}

pub(crate) fn apply_resize<B: Backend>(terminal: &mut Terminal<B>, event: &crossterm::event::Event) -> Result<Option<Size>, B::Error> {
    use crossterm::event::Event;
    let size = match *event {
        Event::Resize(width, height) => Size::new(width, height),
        Event::FocusGained => terminal.size()?,
        _ => return Ok(None),
    };
    if terminal.get_frame().area().as_size() == size { return Ok(None); }
    terminal.resize(Rect::new(0, 0, size.width, size.height))?;
    Ok(Some(size))
}

pub(crate) fn buffered_output<W: Write>(writer: W) -> BufWriter<W> {
    // Enough for a large, densely colored viewport. Larger frames stream in
    // bounded chunks instead of growing memory with the terminal dimensions.
    BufWriter::with_capacity(256 * 1024, writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        backend::{Backend, CrosstermBackend},
        buffer::Buffer,
        layout::Rect,
        style::Color,
    };

    #[derive(Default)]
    struct RecordingWriter {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn underline_color_commands_use_atomic_sgr_subparameters() {
        use crossterm::{Command, style::{Color as C, SetUnderlineColor}};
        crossterm::style::force_color_output(true);
        for (color, expected) in [
            (C::Rgb { r: 255, g: 204, b: 102 }, "\x1b[58:2::255:204:102m"),
            (C::Rgb { r: 0, g: 1, b: 4 }, "\x1b[58:2::0:1:4m"),
            (C::AnsiValue(102), "\x1b[58:5:102m"),
            (C::Red, "\x1b[58:5:9m"),
            (C::Reset, "\x1b[59m"),
        ] {
            let mut ansi = String::new();
            SetUnderlineColor(color).write_ansi(&mut ansi).unwrap();
            assert_eq!(ansi, expected);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_underline_colors_preserve_text_attributes() {
        use std::{fs::OpenOptions, os::windows::{io::AsRawHandle, process::CommandExt}, process::Command};
        use ratatui::style::{Modifier, Style};
        use windows_sys::Win32::System::Console::{
            COORD, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode,
            ReadConsoleOutputAttribute, SetConsoleMode,
        };

        const PROBE: &str = "VTCODE_UNDERLINE_CONSOLE_TEST";
        if std::env::var_os(PROBE).is_none() {
            // Run against an isolated, hidden console, never the user's terminal.
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "terminal_output::tests::windows_underline_colors_preserve_text_attributes", "--nocapture"])
                .env(PROBE, "1")
                .env("TERM", "xterm-256color")
                .env_remove("NO_COLOR")
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output().unwrap();
            assert!(output.status.success(), "{}\n{}",
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }

        let output = OpenOptions::new().read(true).write(true).open("CONOUT$").unwrap();
        crossterm::style::force_color_output(true);
        let handle = output.as_raw_handle();
        let mut mode = 0;
        unsafe {
            assert_ne!(GetConsoleMode(handle, &mut mode), 0);
            assert_ne!(SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
        }
        let mut backend = CrosstermBackend::new(buffered_output(output));
        for underline in [Color::Rgb(255, 204, 102), Color::Rgb(230, 180, 80), Color::Rgb(0, 1, 4), Color::Indexed(102)] {
            let mut cells = Buffer::empty(Rect::new(0, 0, 8, 2));
            for y in 0..2 {
                for (x, ch) in "def main".chars().enumerate() {
                    let style = Style::default()
                        .fg(if x < 3 { Color::Rgb(255, 143, 64) } else { Color::Rgb(166, 226, 46) })
                        .bg(Color::Rgb(30, 36, 44));
                    let style = if x < 3 { style.add_modifier(Modifier::ITALIC) } else { style };
                    let style = if y == 1 { style.add_modifier(Modifier::UNDERLINED).underline_color(underline) } else { style };
                    cells[(x as u16, y)].set_char(ch).set_style(style);
                }
            }
            backend.draw(cells.content.iter().enumerate()
                .map(|(i, cell)| ((i % 8) as u16, (i / 8) as u16, cell))).unwrap();
            Backend::flush(&mut backend).unwrap();
            let mut attributes = [[0u16; 8]; 2];
            for (row, values) in attributes.iter_mut().enumerate() {
                let mut read = 0;
                unsafe {
                    assert_ne!(ReadConsoleOutputAttribute(handle, values.as_mut_ptr(), 8,
                        COORD { X: 0, Y: row as i16 }, &mut read), 0);
                }
                assert_eq!(read, 8);
            }
            for x in 0..8 {
                // ReadConsoleOutputAttribute exposes the nearest ANSI colors.
                // Ignore the underline/italic flags, compare foreground/background.
                assert_eq!(attributes[1][x] & 0xff, attributes[0][x] & 0xff,
                    "underline {underline:?} corrupted text colors at column {x}");
            }
        }
    }

    #[test]
    fn frame_buffer_preserves_ansi_and_batches_stdout_writes() {
        // Both recordings need the same color mode, including under a headless
        // test runner where another ANSI test can enable colors concurrently.
        crossterm::style::force_color_output(true);
        #[cfg(windows)]
        if !crossterm::ansi_support::supports_ansi() {
            eprintln!(
                "ANSI output test needs a VT console or TERM=xterm-256color; legacy WinAPI commands flush immediately"
            );
            return;
        }
        let mut cells = Buffer::empty(Rect::new(0, 0, 160, 50));
        for y in 0..50 {
            for x in 0..160 {
                cells[(x, y)]
                    .set_symbol("x")
                    .set_fg(Color::Rgb((x % 8 * 30) as u8, 100, 180));
            }
        }
        // Stdout is line-buffered. Model that existing layer as well, rather
        // than pretending that each ANSI fragment is a console system call.
        let mut previous = std::io::LineWriter::new(RecordingWriter::default());
        let mut buffered = buffered_output(std::io::LineWriter::new(RecordingWriter::default()));
        for backend in [
            &mut CrosstermBackend::new(&mut previous as &mut dyn Write),
            &mut CrosstermBackend::new(&mut buffered as &mut dyn Write),
        ] {
            backend
                .draw(
                    cells
                        .content
                        .iter()
                        .enumerate()
                        .map(|(i, cell)| ((i % 160) as u16, (i / 160) as u16, cell)),
                )
                .unwrap();
            Backend::flush(backend).unwrap();
        }
        let previous = previous.get_ref();
        let buffered = buffered.get_ref().get_ref();
        assert_eq!(previous.bytes, buffered.bytes);
        assert_eq!(buffered.flushes, 1);
        eprintln!(
            "terminal output: {} bytes, stdout writes {} -> {}",
            previous.bytes.len(),
            previous.writes,
            buffered.writes
        );
        assert!(previous.writes > buffered.writes * 4);
    }
}

#[cfg(test)]
#[path = "terminal_viewport_tests.rs"]
mod viewport_tests;
