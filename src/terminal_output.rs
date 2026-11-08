//! Batch Crossterm's tiny cursor/style/text writes before reaching stdout.
//! Ratatui flushes after each completed frame, and `execute!` flushes commands,
//! so buffering adds no timer or input latency.

use std::io::{BufWriter, Write};
use ratatui::{Terminal, TerminalOptions, Viewport, backend::Backend, layout::{Rect, Size}};

/// Keep the native cursor hidden while a frame diff moves the output cursor.
/// Ratatui requests visibility before setting the final position; apply that
/// request at the frame flush, after the position has actually been restored.
///
/// The terminal's cursor visibility and position are tracked, so an unchanged
/// frame writes nothing, and a changed frame never repeats a hide, show or move
/// that the terminal already reflects.
pub(crate) struct CursorBackend<B> {
    backend: B,
    frame_cursor: Option<bool>,
    visible: Option<bool>,
    position: Option<ratatui::layout::Position>,
}

impl<B> CursorBackend<B> {
    fn new(backend: B) -> Self { Self { backend, frame_cursor: None, visible: None, position: None } }
}

impl<B> std::ops::Deref for CursorBackend<B> {
    type Target = B;
    fn deref(&self) -> &B { &self.backend }
}

impl<B> std::ops::DerefMut for CursorBackend<B> {
    fn deref_mut(&mut self) -> &mut B { &mut self.backend }
}

impl<B: Write> Write for CursorBackend<B> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        // Direct commands (such as the window title) bypass position tracking.
        self.position = None;
        self.backend.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> { Write::flush(&mut self.backend) }
}

impl<B: Backend> Backend for CursorBackend<B> {
    type Error = B::Error;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)> {
        self.frame_cursor = None;
        let mut content = content.peekable();
        if content.peek().is_some() {
            if self.visible != Some(false) {
                self.backend.hide_cursor()?;
                self.visible = Some(false);
            }
            self.position = None;
        }
        self.backend.draw(content)?;
        self.frame_cursor = Some(false);
        Ok(())
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        if let Some(visible) = &mut self.frame_cursor { *visible = false; return Ok(()); }
        self.visible = None;
        self.backend.hide_cursor()?;
        self.visible = Some(false);
        Ok(())
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        if let Some(visible) = &mut self.frame_cursor { *visible = true; return Ok(()); }
        self.visible = None;
        self.backend.show_cursor()?;
        self.visible = Some(true);
        Ok(())
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.backend.get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        if self.frame_cursor.is_some() && self.position == Some(position) { return Ok(()); }
        self.position = None;
        self.backend.set_cursor_position(position).inspect_err(|_| {
            self.frame_cursor = None;
            self.visible = None;
        })?;
        self.position = Some(position);
        Ok(())
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.position = None;
        self.backend.clear()
    }
    fn clear_region(&mut self, region: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.position = None;
        self.backend.clear_region(region)
    }
    fn append_lines(&mut self, count: u16) -> Result<(), Self::Error> {
        self.position = None;
        self.backend.append_lines(count)
    }
    fn size(&self) -> Result<Size, Self::Error> { self.backend.size() }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> { self.backend.window_size() }
    fn flush(&mut self) -> Result<(), Self::Error> {
        if let Some(visible) = self.frame_cursor.take()
            && self.visible != Some(visible)
        {
            self.visible = None;
            if visible { self.backend.show_cursor()?; } else { self.backend.hide_cursor()?; }
            self.visible = Some(visible);
        }
        self.backend.flush()
    }
}

/// A Crossterm-compatible backend that encodes frame diffs compactly.
///
/// Ratatui's Crossterm backend resends both colors whenever either changes,
/// always uses absolute cursor addressing, formats every symbol through `fmt`,
/// and flushes separately for every cursor command. Syntax highlighting mostly
/// changes the foreground over a constant background, so redundant background
/// colors dominated frame bytes. Fewer bytes are less work for ConPTY and the
/// terminal's parser, and much less traffic when the UI runs over SSH.
///
/// Cursor commands are queued into the same write as the frame they belong
/// to, so a frame reaches the terminal in one write, at `flush`. A changed
/// frame is bracketed as a synchronized update (DEC mode 2026): supporting
/// terminals present it atomically instead of tearing mid-scroll or while a
/// large frame crosses a slow SSH link. Other terminals ignore the mode.
pub(crate) struct AnsiBackend<W: Write> {
    writer: W,
    synchronized: bool,
}

const BEGIN_SYNCHRONIZED_UPDATE: &[u8] = b"\x1b[?2026h";
const END_SYNCHRONIZED_UPDATE: &[u8] = b"\x1b[?2026l";
impl<W: Write> AnsiBackend<W> {
    pub(crate) fn new(writer: W) -> Self { Self { writer, synchronized: false } }
}

impl<W: Write> Write for AnsiBackend<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.writer.write(bytes) }
    fn flush(&mut self) -> std::io::Result<()> { self.writer.flush() }
}

fn ansi_supported() -> bool {
    #[cfg(windows)]
    { crossterm::ansi_support::supports_ansi() }
    #[cfg(not(windows))]
    { true }
}

fn push_decimal(out: &mut [u8; 32], len: &mut usize, value: u16) {
    let mut digits = [0u8; 5];
    let mut count = 0;
    let mut value = value;
    loop {
        digits[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 { break; }
    }
    for &digit in digits[..count].iter().rev() {
        out[*len] = digit;
        *len += 1;
    }
}

/// Absolute (`CUP`) or same-row forward (`CUF`) cursor movement, whichever is
/// shorter. `from` is the terminal cursor, known only after an ASCII symbol.
fn write_move(writer: &mut impl Write, from: Option<(u16, u16)>, x: u16, y: u16) -> std::io::Result<()> {
    let mut out = [0u8; 32];
    out[0] = 0x1b;
    out[1] = b'[';
    let mut len = 2;
    match from {
        Some((column, row)) if row == y && x > column => {
            let distance = x - column;
            if distance > 1 { push_decimal(&mut out, &mut len, distance); }
            out[len] = b'C';
            len += 1;
        }
        _ => {
            push_decimal(&mut out, &mut len, y.saturating_add(1));
            out[len] = b';';
            len += 1;
            push_decimal(&mut out, &mut len, x.saturating_add(1));
            out[len] = b'H';
            len += 1;
        }
    }
    writer.write_all(&out[..len])
}

/// The same attribute sequence as Ratatui's Crossterm backend, including
/// re-applying the remaining intensity after a NormalIntensity reset.
fn queue_modifier_diff(
    writer: &mut impl Write,
    from: ratatui::style::Modifier,
    to: ratatui::style::Modifier,
) -> std::io::Result<()> {
    use crossterm::{queue, style::{Attribute, SetAttribute}};
    use ratatui::style::Modifier;
    let removed = from - to;
    if removed.contains(Modifier::REVERSED) { queue!(writer, SetAttribute(Attribute::NoReverse))?; }
    if removed.intersects(Modifier::BOLD | Modifier::DIM) {
        queue!(writer, SetAttribute(Attribute::NormalIntensity))?;
        if to.contains(Modifier::DIM) { queue!(writer, SetAttribute(Attribute::Dim))?; }
        if to.contains(Modifier::BOLD) { queue!(writer, SetAttribute(Attribute::Bold))?; }
    }
    if removed.contains(Modifier::ITALIC) { queue!(writer, SetAttribute(Attribute::NoItalic))?; }
    if removed.contains(Modifier::UNDERLINED) { queue!(writer, SetAttribute(Attribute::NoUnderline))?; }
    if removed.contains(Modifier::CROSSED_OUT) { queue!(writer, SetAttribute(Attribute::NotCrossedOut))?; }
    if removed.intersects(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK) {
        queue!(writer, SetAttribute(Attribute::NoBlink))?;
    }
    let added = to - from;
    if added.contains(Modifier::REVERSED) { queue!(writer, SetAttribute(Attribute::Reverse))?; }
    if added.contains(Modifier::BOLD) { queue!(writer, SetAttribute(Attribute::Bold))?; }
    if added.contains(Modifier::ITALIC) { queue!(writer, SetAttribute(Attribute::Italic))?; }
    if added.contains(Modifier::UNDERLINED) { queue!(writer, SetAttribute(Attribute::Underlined))?; }
    if added.contains(Modifier::DIM) { queue!(writer, SetAttribute(Attribute::Dim))?; }
    if added.contains(Modifier::CROSSED_OUT) { queue!(writer, SetAttribute(Attribute::CrossedOut))?; }
    if added.contains(Modifier::SLOW_BLINK) { queue!(writer, SetAttribute(Attribute::SlowBlink))?; }
    if added.contains(Modifier::RAPID_BLINK) { queue!(writer, SetAttribute(Attribute::RapidBlink))?; }
    Ok(())
}

impl<W: Write> Backend for AnsiBackend<W> {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)> {
        use crossterm::{queue, style::{Attribute, Colors, SetAttribute, SetColors, SetUnderlineColor}};
        use ratatui::{backend::CrosstermBackend, style::{Color, Modifier}};
        use ratatui::backend::IntoCrossterm;
        if !ansi_supported() {
            return CrosstermBackend::new(&mut self.writer).draw(content);
        }
        let mut content = content.peekable();
        if content.peek().is_some() && !self.synchronized {
            self.writer.write_all(BEGIN_SYNCHRONIZED_UPDATE)?;
            self.synchronized = true;
        }
        let writer = &mut self.writer;
        let (mut fg, mut bg, mut underline) = (Color::Reset, Color::Reset, Color::Reset);
        let mut modifier = Modifier::empty();
        // Ratatui assumes every symbol advances one column. Keep that rule for
        // adjacent cells, but measure a relative jump only from an ASCII cell.
        let mut last: Option<(u16, u16)> = None;
        let mut known: Option<(u16, u16)> = None;
        for (x, y, cell) in content {
            if !matches!(last, Some((column, row)) if x == column.wrapping_add(1) && y == row) {
                write_move(writer, known, x, y)?;
            }
            last = Some((x, y));
            if cell.modifier != modifier {
                queue_modifier_diff(writer, modifier, cell.modifier)?;
                modifier = cell.modifier;
            }
            let foreground = (cell.fg != fg).then(|| cell.fg.into_crossterm());
            let background = (cell.bg != bg).then(|| cell.bg.into_crossterm());
            if foreground.is_some() || background.is_some() {
                queue!(writer, SetColors(Colors { foreground, background }))?;
                fg = cell.fg;
                bg = cell.bg;
            }
            if cell.underline_color != underline {
                queue!(writer, SetUnderlineColor(cell.underline_color.into_crossterm()))?;
                underline = cell.underline_color;
            }
            let symbol = cell.symbol().as_bytes();
            writer.write_all(symbol)?;
            known = matches!(symbol, [byte] if *byte >= b' ' && *byte < 0x7f).then(|| (x.saturating_add(1), y));
        }
        // Each frame starts from the default style; restore only what changed.
        if fg != Color::Reset || bg != Color::Reset {
            queue!(writer, SetColors(Colors {
                foreground: (fg != Color::Reset).then_some(crossterm::style::Color::Reset),
                background: (bg != Color::Reset).then_some(crossterm::style::Color::Reset),
            }))?;
        }
        if underline != Color::Reset { queue!(writer, SetUnderlineColor(crossterm::style::Color::Reset))?; }
        if !modifier.is_empty() { queue!(writer, SetAttribute(Attribute::Reset))?; }
        Ok(())
    }
    fn hide_cursor(&mut self) -> std::io::Result<()> {
        crossterm::queue!(self.writer, crossterm::cursor::Hide)
    }
    fn show_cursor(&mut self) -> std::io::Result<()> {
        crossterm::queue!(self.writer, crossterm::cursor::Show)
    }
    fn get_cursor_position(&mut self) -> std::io::Result<ratatui::layout::Position> {
        Write::flush(&mut self.writer)?;
        ratatui::backend::CrosstermBackend::new(&mut self.writer).get_cursor_position()
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(&mut self, position: P) -> std::io::Result<()> {
        let position = position.into();
        if !ansi_supported() {
            return ratatui::backend::CrosstermBackend::new(&mut self.writer).set_cursor_position(position);
        }
        write_move(&mut self.writer, None, position.x, position.y)
    }
    fn clear(&mut self) -> std::io::Result<()> {
        ratatui::backend::CrosstermBackend::new(&mut self.writer).clear()
    }
    fn clear_region(&mut self, region: ratatui::backend::ClearType) -> std::io::Result<()> {
        ratatui::backend::CrosstermBackend::new(&mut self.writer).clear_region(region)
    }
    fn append_lines(&mut self, count: u16) -> std::io::Result<()> {
        ratatui::backend::CrosstermBackend::new(&mut self.writer).append_lines(count)
    }
    fn size(&self) -> std::io::Result<Size> {
        let (width, height) = crossterm::terminal::size()?;
        Ok(Size::new(width, height))
    }
    fn window_size(&mut self) -> std::io::Result<ratatui::backend::WindowSize> {
        ratatui::backend::CrosstermBackend::new(&mut self.writer).window_size()
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if std::mem::take(&mut self.synchronized) {
            self.writer.write_all(END_SYNCHRONIZED_UPDATE)?;
        }
        self.writer.flush()
    }
}

/// Resize packets already contain the terminal dimensions. Polling the console
/// host from every draw can take a whole scheduling quantum on a busy Windows
/// machine; use those packets and reconcile once when focus returns instead.
pub(crate) fn event_driven_terminal<B: Backend>(backend: B) -> Result<Terminal<CursorBackend<B>>, B::Error> {
    let size = backend.size()?;
    Terminal::with_options(CursorBackend::new(backend), TerminalOptions {
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

#[cfg(test)]
pub(crate) fn test_cursor_backend<B>(backend: B) -> CursorBackend<B> { CursorBackend::new(backend) }

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

    #[derive(Clone, Default)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    fn symbol_is_blank_run(next: &mut impl FnMut(u64) -> u64) -> bool { next(3) == 0 }

    fn screen(terminal: &crate::terminal::TerminalEmulator, height: usize) -> Vec<Vec<crate::terminal::TermCell>> {
        (0..height).map(|row| terminal.grid.display_line(row).to_vec()).collect()
    }

    /// Replays identical frame diffs through Ratatui's Crossterm backend and the
    /// compact backend in a VT emulator; every screen state must match.
    #[test]
    fn compact_backend_reproduces_crossterm_screens_with_fewer_bytes() {
        use ratatui::style::{Modifier, Style};
        crossterm::style::force_color_output(true);
        #[cfg(windows)]
        if !crossterm::ansi_support::supports_ansi() { return; }
        let (width, height) = (37u16, 9u16);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |bound: u64| {
            seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
            seed % bound
        };
        let colors = [Color::Reset, Color::Rgb(30, 36, 44), Color::Rgb(255, 143, 64), Color::Indexed(102),
            Color::Red, Color::LightCyan, Color::Rgb(0, 0, 0)];
        let modifiers = [Modifier::empty(), Modifier::BOLD, Modifier::DIM, Modifier::ITALIC | Modifier::UNDERLINED,
            Modifier::BOLD | Modifier::DIM, Modifier::REVERSED, Modifier::CROSSED_OUT];
        let symbols = ["a", " ", " ", " ", "x", "│", "é", "─", "Z", "{"];
        let area = Rect::new(0, 0, width, height);
        let mut previous = Buffer::empty(area);
        let mut old_screen = crate::terminal::TerminalEmulator::test_with_output(width.into(), height.into(), "");
        let mut new_screen = crate::terminal::TerminalEmulator::test_with_output(width.into(), height.into(), "");
        let (mut old_total, mut new_total) = (0, 0);
        for frame in 0..300 {
            let mut current = previous.clone();
            // Mostly local edits with long runs; occasionally a full repaint.
            let changes = if frame % 50 == 0 { usize::from(width * height) } else { next(40) as usize };
            for _ in 0..changes {
                let (x, y) = (next(width.into()) as u16, next(height.into()) as u16);
                let long = symbol_is_blank_run(&mut next);
                let run = 1 + next(if long { 40 } else { 12 }) as u16;
                let style = Style::default()
                    .fg(colors[next(colors.len() as u64) as usize])
                    .bg(colors[next(colors.len() as u64) as usize])
                    .add_modifier(modifiers[next(modifiers.len() as u64) as usize]);
                let style = if next(5) == 0 { style.underline_color(colors[next(colors.len() as u64) as usize]) } else { style };
                let symbol = symbols[next(symbols.len() as u64) as usize];
                for column in x..(x + run).min(width) {
                    current[(column, y)].set_symbol(symbol).set_style(style);
                }
            }
            let updates = previous.diff(&current);
            let old = Shared::default();
            let new = Shared::default();
            CrosstermBackend::new(old.clone()).draw(updates.iter().copied()).unwrap();
            AnsiBackend::new(new.clone()).draw(updates.iter().copied()).unwrap();
            let cursor = (next(width.into()) as u16, next(height.into()) as u16);
            CrosstermBackend::new(old.clone()).set_cursor_position(cursor).unwrap();
            AnsiBackend::new(new.clone()).set_cursor_position(cursor).unwrap();
            let (old, new) = (old.0.take(), new.0.take());
            old_total += old.len();
            new_total += new.len();
            old_screen.test_queue_output(&old);
            new_screen.test_queue_output(&new);
            old_screen.process_while(|| true);
            new_screen.process_while(|| true);
            let (expected, actual) = (screen(&old_screen, height.into()), screen(&new_screen, height.into()));
            if let Some((row, column)) = (0..usize::from(height)).flat_map(|row| (0..usize::from(width)).map(move |column| (row, column)))
                .find(|&(row, column)| expected[row][column] != actual[row][column])
            {
                panic!("frame {frame} differs at {row},{column}: {:?} vs {:?}
old={:?}
new={:?}",
                    expected[row][column], actual[row][column], String::from_utf8_lossy(&old), String::from_utf8_lossy(&new));
            }
            assert_eq!((old_screen.grid.cursor_row, old_screen.grid.cursor_col),
                (new_screen.grid.cursor_row, new_screen.grid.cursor_col), "frame {frame} cursor");
            previous = current;
        }
        eprintln!("compact backend: {old_total} -> {new_total} bytes");
        assert!(new_total < old_total);
    }

    #[test]
    fn unchanged_frames_write_nothing_and_cursor_commands_share_the_frame_write() {
        crossterm::style::force_color_output(true);
        #[cfg(windows)]
        if !crossterm::ansi_support::supports_ansi() { return; }
        let bytes = Shared::default();
        let writes = std::rc::Rc::new(std::cell::Cell::new(0usize));
        struct Counting(Shared, std::rc::Rc<std::cell::Cell<usize>>);
        impl Write for Counting {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> { self.0.write(data) }
            fn flush(&mut self) -> std::io::Result<()> { self.1.set(self.1.get() + 1); Ok(()) }
        }
        let backend = CursorBackend::new(AnsiBackend::new(Counting(bytes.clone(), writes.clone())));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, 20, 4)) }).unwrap();
        let paint = |terminal: &mut Terminal<_>, text: &str, cursor: Option<(u16, u16)>| {
            terminal.draw(|frame| {
                frame.render_widget(ratatui::widgets::Paragraph::new(text), frame.area());
                if let Some(cursor) = cursor { frame.set_cursor_position(cursor); }
            }).unwrap();
        };
        let before = writes.get();
        paint(&mut terminal, "hello", Some((2, 1)));
        let first = bytes.0.take();
        assert!(!first.is_empty());
        assert_eq!(writes.get() - before, 1, "one terminal write per frame");
        // Same content and caret: nothing reaches the terminal.
        paint(&mut terminal, "hello", Some((2, 1)));
        assert!(bytes.0.borrow().is_empty(), "{:?}", String::from_utf8_lossy(&bytes.0.borrow()));
        // Only the caret moves: one move command, no hide/show round trip.
        paint(&mut terminal, "hello", Some((3, 1)));
        assert_eq!(bytes.0.take(), b"\x1b[2;4H");
        // Content changes under a visible caret: hide, paint, restore.
        paint(&mut terminal, "help", Some((3, 1)));
        let changed = String::from_utf8(bytes.0.take()).unwrap();
        assert!(changed.starts_with("\x1b[?25l\x1b[?2026h") && changed.ends_with("\x1b[2;4H\x1b[?25h\x1b[?2026l"), "{changed:?}");
        // Hiding an already hidden caret costs nothing.
        paint(&mut terminal, "help", None);
        assert_eq!(bytes.0.take(), b"\x1b[?25l");
        paint(&mut terminal, "help", None);
        assert!(bytes.0.borrow().is_empty());
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

#[cfg(all(test, windows))]
#[path = "terminal_cursor_tests.rs"]
mod cursor_tests;
