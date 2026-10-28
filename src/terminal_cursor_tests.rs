//! Observe actual console cursor state after each output write, including the
//! intermediate flushes inside Ratatui, rather than just the completed frame.
use super::*;
use std::{
    cell::{Cell, RefCell}, fs::{File, OpenOptions}, os::windows::{io::AsRawHandle, process::CommandExt},
    process::Command, rc::Rc, time::{Duration, Instant},
};
use ratatui::{backend::CrosstermBackend, layout::Position};
use windows_sys::Win32::System::Console::{
    CONSOLE_CURSOR_INFO, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    GetConsoleCursorInfo, GetConsoleMode, GetConsoleScreenBufferInfo, SetConsoleMode,
};

struct CursorProbe {
    output: File,
    observing: Rc<Cell<bool>>,
    expected: Rc<Cell<Option<Position>>>,
    misplaced: Rc<RefCell<Vec<Position>>>,
}

impl Write for CursorProbe {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.output.write(bytes)?;
        if self.observing.get() {
            let (visible, position) = cursor(&self.output);
            if visible && Some(position) != self.expected.get() {
                self.misplaced.borrow_mut().push(position);
            }
        }
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.output.flush() }
}

fn cursor(output: &File) -> (bool, Position) {
    let mut info = CONSOLE_CURSOR_INFO::default();
    let mut screen = CONSOLE_SCREEN_BUFFER_INFO::default();
    // Both APIs write initialized structures using this live console handle.
    unsafe {
        assert_ne!(GetConsoleCursorInfo(output.as_raw_handle(), &mut info), 0);
        assert_ne!(GetConsoleScreenBufferInfo(output.as_raw_handle(), &mut screen), 0);
    }
    (info.bVisible != 0, Position::new(screen.dwCursorPosition.X as u16, screen.dwCursorPosition.Y as u16))
}

#[test]
fn windows_loading_output_keeps_cursor_at_its_input_position() {
    const PROBE: &str = "VTCODE_CURSOR_PAINT_PROBE";
    if std::env::var_os(PROBE).is_none() {
        // An isolated, hidden console leaves the user's terminal untouched.
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "terminal_output::cursor_tests::windows_loading_output_keeps_cursor_at_its_input_position", "--nocapture"])
            .env(PROBE, "1").env("TERM", "xterm-256color").env_remove("NO_COLOR")
            .creation_flags(0x0800_0000).output().unwrap(); // CREATE_NO_WINDOW
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return;
    }
    let output = OpenOptions::new().read(true).write(true).open("CONOUT$").unwrap();
    let inspect = output.try_clone().unwrap();
    let mut mode = 0;
    unsafe {
        assert_ne!(GetConsoleMode(output.as_raw_handle(), &mut mode), 0);
        assert_ne!(SetConsoleMode(output.as_raw_handle(), mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
    }
    let observing = Rc::new(Cell::new(true));
    let expected = Rc::new(Cell::new(None));
    let misplaced = Rc::new(RefCell::new(Vec::new()));
    let probe = CursorProbe { output, observing: observing.clone(), expected: expected.clone(), misplaced: misplaced.clone() };
    let backend = CursorBackend::new(CrosstermBackend::new(buffered_output(probe)));
    let mut terminal = Terminal::with_options(backend, TerminalOptions {
        viewport: Viewport::Fixed(Rect::new(0, 0, 80, 12)),
    }).unwrap();
    let mut app = crate::app::App::test_new();
    let started = Instant::now();
    for tick in 0..36 {
        let now = started + Duration::from_millis(tick * 80);
        if tick > 0 { assert!(app.loading.advance(true, now)); }
        app.loading.begin_frame();
        let caret = if tick < 12 { Some(Position::new(50, 8)) }
            else if tick < 24 { None } else { Some(Position::new(60, 9)) };
        expected.set(caret);
        terminal.draw(|frame| {
            crate::ui_loading::draw_header(frame, &mut app, Rect::new(1, 0, 24, 1));
            if let Some(caret) = caret { frame.set_cursor_position(caret); }
        }).unwrap();
        app.loading.finish_frame(now);
        let (visible, position) = cursor(&inspect);
        assert_eq!(visible, caret.is_some());
        if let Some(caret) = caret { assert_eq!(position, caret); }
    }
    observing.set(false);
    assert!(misplaced.borrow().is_empty(), "cursor appeared away from its input: {:?}", misplaced.borrow());
}
