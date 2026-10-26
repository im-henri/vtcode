//! Preserve the caller's console mode and let Windows Terminal deliver paste
//! delimiters. The local Crossterm patch assembles those records into Paste.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::System::Console::{
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleCP,
    GetConsoleMode, GetConsoleOutputCP, SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
};

const UTF8_CODE_PAGE: u32 = 65001;

pub(crate) struct ConsoleInputMode {
    input: File,
    original: u32,
    output: File,
    original_output: u32,
    original_input_code_page: u32,
    original_output_code_page: u32,
    win32_enabled: std::cell::Cell<bool>,
    alternate_screen: std::cell::Cell<bool>,
}

impl ConsoleInputMode {
    pub(crate) fn save() -> io::Result<Self> {
        // Match Crossterm's CONIN$ handle, including when stdin is redirected.
        let input = OpenOptions::new().read(true).write(true).open("CONIN$")?;
        let original = Self::mode(&input)?;
        let output = OpenOptions::new().read(true).write(true).open("CONOUT$")?;
        let original_output = Self::mode(&output)?;
        // SAFETY: these APIs read code pages of this process's console.
        let original_input_code_page = unsafe { GetConsoleCP() };
        let original_output_code_page = unsafe { GetConsoleOutputCP() };
        if original_input_code_page == 0 || original_output_code_page == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            input,
            original,
            output,
            original_output,
            original_input_code_page,
            original_output_code_page,
            win32_enabled: std::cell::Cell::new(false),
            alternate_screen: std::cell::Cell::new(false),
        })
    }

    fn mode(input: &File) -> io::Result<u32> {
        let mut mode = 0;
        // SAFETY: input owns a live handle and mode points to writable storage.
        if unsafe { GetConsoleMode(input.as_raw_handle(), &mut mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(mode)
    }

    /// Called only after EnterAlternateScreen has completed. That buffer has
    /// no scrollback: its mouse origin stays zero through resizing and focus
    /// changes. Ordinary console users keep Crossterm's live viewport lookup.
    pub(crate) fn use_alternate_screen_coordinates(&self) {
        crossterm::event::set_windows_mouse_viewport_origin(Some(0));
        self.alternate_screen.set(true);
    }

    pub(crate) fn enable_virtual_terminal_input(&self) -> io::Result<()> {
        // ConPTY's legacy key translation uses console code pages. Preserve
        // Unicode paste even when the launching shell uses an OEM code page.
        // SAFETY: both setters operate on this process's attached console.
        if unsafe { SetConsoleCP(UTF8_CODE_PAGE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetConsoleOutputCP(UTF8_CODE_PAGE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mode = Self::mode(&self.input)? | ENABLE_VIRTUAL_TERMINAL_INPUT;
        // SAFETY: the file owns this console handle for the entire call.
        if unsafe { SetConsoleMode(self.input.as_raw_handle(), mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // VT input alone collapses Ctrl+Shift+letter into Ctrl+letter. Request
        // Windows' lossless key records from the console, including ConPTY.
        let output_mode = Self::mode(&self.output)? | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        if unsafe { SetConsoleMode(self.output.as_raw_handle(), output_mode) } == 0 {
            return Err(io::Error::last_os_error());
        }
        self.win32_enabled.set(true);
        let mut output = &self.output;
        output.write_all(b"\x1b[?9001h")?;
        output.flush()?;
        crossterm::event::set_windows_win32_input_mode(true);
        Ok(())
    }

    /// SSH reads and manages this original console itself. Prepare Unicode
    /// output without requesting our native editor's Win32 keyboard transport.
    pub(crate) fn prepare_bridge(&self) -> io::Result<()> {
        if unsafe { SetConsoleCP(UTF8_CODE_PAGE) } == 0
            || unsafe { SetConsoleOutputCP(UTF8_CODE_PAGE) } == 0
        { return Err(io::Error::last_os_error()); }
        let mode = Self::mode(&self.output)? | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            | windows_sys::Win32::System::Console::DISABLE_NEWLINE_AUTO_RETURN;
        if unsafe { SetConsoleMode(self.output.as_raw_handle(), mode) } == 0 { return Err(io::Error::last_os_error()); }
        let mut output = &self.output;
        let mode = Self::mode(&self.input)?;
        let mode = (mode & !(1 | 2 | 4 | 0x40)) | ENABLE_VIRTUAL_TERMINAL_INPUT
            | windows_sys::Win32::System::Console::ENABLE_WINDOW_INPUT;
        if unsafe { SetConsoleMode(self.input.as_raw_handle(), mode) } == 0 { return Err(io::Error::last_os_error()); }
        output.write_all(b"\x1b[?9001l")?;
        output.flush()
    }
}

impl Drop for ConsoleInputMode {
    fn drop(&mut self) {
        if self.alternate_screen.get() {
            crossterm::event::set_windows_mouse_viewport_origin(None);
        }
        if self.win32_enabled.get() {
            let _ = self.output.write_all(b"\x1b[?9001l");
            let _ = self.output.flush();
            crossterm::event::set_windows_win32_input_mode(false);
        }
        // Restore even after setup errors or panic unwinding. Cleanup is best effort.
        unsafe { SetConsoleMode(self.input.as_raw_handle(), self.original) };
        unsafe { SetConsoleMode(self.output.as_raw_handle(), self.original_output) };
        unsafe { SetConsoleCP(self.original_input_code_page) };
        unsafe { SetConsoleOutputCP(self.original_output_code_page) };
    }
}

#[cfg(test)]
#[path = "windows_paste_memory_tests.rs"]
mod paste_memory_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    use std::time::Duration;

    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
    use windows_sys::Win32::System::Console::{
        INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0,
        WriteConsoleInputW,
    };

    fn key(unit: u16, virtual_key: u16, down: bool, repeats: u16) -> INPUT_RECORD {
        INPUT_RECORD {
            EventType: KEY_EVENT as u16,
            Event: INPUT_RECORD_0 {
                KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: i32::from(down),
                    wRepeatCount: repeats,
                    wVirtualKeyCode: virtual_key,
                    wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
                    dwControlKeyState: 0,
                },
            },
        }
    }

    fn inject(input: &File, records: &[INPUT_RECORD]) {
        let mut written = 0;
        // SAFETY: records and written are valid for the supplied lengths.
        assert_ne!(
            unsafe {
                WriteConsoleInputW(
                    input.as_raw_handle(),
                    records.as_ptr(),
                    records.len() as u32,
                    &mut written,
                )
            },
            0
        );
        assert_eq!(written as usize, records.len());
    }

    fn inject_text(input: &File, text: &str) {
        let records: Vec<_> = text.encode_utf16().map(|u| key(u, 0, true, 1)).collect();
        inject(input, &records);
    }

    fn inject_raw_text(input: &File, text: &str) {
        // Model modern OpenConsole's WriteStringRaw path. Normal
        // WriteConsoleInputW encodes records while VT input is enabled; turn
        // that conversion off only while enqueueing these raw records.
        let mode = ConsoleInputMode::mode(input).unwrap();
        unsafe {
            assert_ne!(
                SetConsoleMode(input.as_raw_handle(), mode & !ENABLE_VIRTUAL_TERMINAL_INPUT),
                0
            );
        }
        inject_text(input, text);
        unsafe {
            assert_ne!(SetConsoleMode(input.as_raw_handle(), mode), 0);
        }
    }

    fn next() -> Event {
        assert!(
            event::poll(Duration::from_secs(2)).unwrap(),
            "input event timed out"
        );
        event::read().unwrap()
    }

    #[test]
    fn zero_timeout_poll_recognizes_already_buffered_vt_mouse_and_keyboard_packets() {
        const PROBE: &str = "VTCODE_READY_VT_INPUT_PROBE";
        if std::env::var_os(PROBE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows_input::tests::zero_timeout_poll_recognizes_already_buffered_vt_mouse_and_keyboard_packets", "--nocapture"])
                .env(PROBE, "1")
                .creation_flags(0x0800_0000)
                .output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        let mode = ConsoleInputMode::save().unwrap();
        crossterm::terminal::enable_raw_mode().unwrap();
        mode.enable_virtual_terminal_input().unwrap();
        inject_raw_text(&mode.input, "\x1b[<32;15;8M\x1b[83;31;19;1;8;1_");
        assert!(event::poll(Duration::ZERO).unwrap(), "a complete queued VT mouse packet is ready, even when it spans several native records");
        assert!(matches!(event::read().unwrap(), Event::Mouse(_)));
        assert!(event::poll(Duration::ZERO).unwrap(), "a complete queued Win32 key packet is ready");
        assert_eq!(event::read().unwrap(), Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        assert!(!event::poll(Duration::ZERO).unwrap());
        // An incomplete packet must never cause zero-time polling to wait.
        inject_raw_text(&mode.input, "\x1b[<32;15;");
        assert!(!event::poll(Duration::ZERO).unwrap());
        inject_raw_text(&mode.input, "8M");
        assert!(event::poll(Duration::ZERO).unwrap());
        assert!(matches!(event::read().unwrap(), Event::Mouse(_)));
    }

    fn check_side_button_input(input: &File) {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use windows_sys::Win32::System::Console::{COORD, MOUSE_EVENT, MOUSE_EVENT_RECORD};

        for (mask, code, button) in [(8, 128, MouseButton::Back), (16, 129, MouseButton::Forward)] {
            for (state, kind) in [(mask, MouseEventKind::Down(button)), (0, MouseEventKind::Up(button))] {
                let mode = ConsoleInputMode::mode(input).unwrap();
                unsafe { assert_ne!(SetConsoleMode(input.as_raw_handle(), mode & !ENABLE_VIRTUAL_TERMINAL_INPUT), 0); }
                inject(input, &[INPUT_RECORD {
                    EventType: MOUSE_EVENT as u16,
                    Event: INPUT_RECORD_0 { MouseEvent: MOUSE_EVENT_RECORD {
                        dwMousePosition: COORD { X: 7, Y: 0 },
                        dwButtonState: state, dwControlKeyState: 0, dwEventFlags: 0,
                    } },
                }]);
                unsafe { assert_ne!(SetConsoleMode(input.as_raw_handle(), mode), 0); }
                assert_eq!(next(), Event::Mouse(MouseEvent {
                    kind, column: 7, row: 0, modifiers: KeyModifiers::NONE,
                }));
            }
            for (suffix, kind) in [('M', MouseEventKind::Down(button)), ('m', MouseEventKind::Up(button))] {
                inject_raw_text(input, &format!("\x1b[<{code};8;1{suffix}"));
                assert_eq!(next(), Event::Mouse(MouseEvent {
                    kind, column: 7, row: 0, modifiers: KeyModifiers::NONE,
                }));
            }
        }
    }

    fn check_control_hover_input(input: &File) {
        use crossterm::event::{KeyEventKind, ModifierKeyCode};
        for (enhanced, code) in [(0, ModifierKeyCode::LeftControl), (256, ModifierKeyCode::RightControl)] {
            for (down, kind, flags, modifiers) in [
                (true, KeyEventKind::Press, if enhanced == 0 { 8 } else { 4 }, KeyModifiers::CONTROL),
                (false, KeyEventKind::Release, 0, KeyModifiers::NONE),
            ] {
                let mode = ConsoleInputMode::mode(input).unwrap();
                unsafe { assert_ne!(SetConsoleMode(input.as_raw_handle(), mode & !ENABLE_VIRTUAL_TERMINAL_INPUT), 0); }
                let mut record = key(0, 17, down, 1);
                record.Event.KeyEvent.dwControlKeyState = flags | enhanced;
                inject(input, &[record]);
                unsafe { assert_ne!(SetConsoleMode(input.as_raw_handle(), mode), 0); }
                let expected = Event::Key(KeyEvent::new_with_kind(KeyCode::Modifier(code), modifiers, kind));
                assert_eq!(next(), expected);
                inject_raw_text(input, &format!("\x1b[17;29;0;{};{};1_", u8::from(down), flags | enhanced));
                assert_eq!(next(), expected);
            }
        }
    }

    fn check_shift_mouse_input(input: &File) {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use windows_sys::Win32::System::Console::{
            COORD, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED, MOUSE_WHEELED, SHIFT_PRESSED,
        };

        // PowerShell in a classic console delivers native records; WezTerm
        // delivers SGR packets. Both must keep Shift through the real reader.
        for (buttons, flags, code, suffix, kind) in [
            (1, 0, 4, 'M', MouseEventKind::Down(MouseButton::Left)),
            (1, MOUSE_MOVED, 36, 'M', MouseEventKind::Drag(MouseButton::Left)),
            (0, 0, 4, 'm', MouseEventKind::Up(MouseButton::Left)),
            (120 << 16, MOUSE_WHEELED, 68, 'M', MouseEventKind::ScrollUp),
            ((-120i16 as u16 as u32) << 16, MOUSE_WHEELED, 69, 'M', MouseEventKind::ScrollDown),
        ] {
            let mode = ConsoleInputMode::mode(input).unwrap();
            unsafe { assert_ne!(SetConsoleMode(input.as_raw_handle(), mode & !ENABLE_VIRTUAL_TERMINAL_INPUT), 0); }
            inject(input, &[INPUT_RECORD {
                EventType: MOUSE_EVENT as u16,
                Event: INPUT_RECORD_0 { MouseEvent: MOUSE_EVENT_RECORD {
                    dwMousePosition: COORD { X: 7, Y: 0 }, dwButtonState: buttons,
                    dwControlKeyState: SHIFT_PRESSED, dwEventFlags: flags,
                } },
            }]);
            unsafe { assert_ne!(SetConsoleMode(input.as_raw_handle(), mode), 0); }
            let expected = Event::Mouse(MouseEvent { kind, column: 7, row: 0, modifiers: KeyModifiers::SHIFT });
            assert_eq!(next(), expected);
            inject_raw_text(input, &format!("\x1b[<{code};8;1{suffix}"));
            assert_eq!(next(), expected);
        }
    }

    #[test]
    fn windows_console_pointer_batches_preserve_keys_releases_and_paste_boundaries() {
        const PROBE: &str = "VTCODE_POINTER_BATCH_CONSOLE_TEST";
        if std::env::var_os(PROBE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows_input::tests::windows_console_pointer_batches_preserve_keys_releases_and_paste_boundaries", "--nocapture"])
                .env(PROBE, "1")
                .creation_flags(0x0800_0000)
                .output().unwrap();
            assert!(output.status.success(), "{}\n{}",
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use windows_sys::Win32::System::Console::{COORD, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED};
        let mode = ConsoleInputMode::save().unwrap();
        crossterm::terminal::enable_raw_mode().unwrap();
        mode.enable_virtual_terminal_input().unwrap();
        let mut records = Vec::new();
        let mut expected = Vec::new();
        for index in 0..1_026u16 {
            let release = index == 1_025;
            let column = index * 13 % 70;
            let row = index * 7 % 30;
            records.push(INPUT_RECORD {
                EventType: MOUSE_EVENT as u16,
                Event: INPUT_RECORD_0 { MouseEvent: MOUSE_EVENT_RECORD {
                    dwMousePosition: COORD { X: column as i16, Y: row as i16 },
                    dwButtonState: u32::from(!release), dwControlKeyState: 0,
                    dwEventFlags: if index == 0 || release { 0 } else { MOUSE_MOVED },
                } },
            });
            expected.push(Event::Mouse(MouseEvent {
                kind: if index == 0 { MouseEventKind::Down(MouseButton::Left) }
                    else if release { MouseEventKind::Up(MouseButton::Left) }
                    else { MouseEventKind::Drag(MouseButton::Left) },
                column, row, modifiers: KeyModifiers::NONE,
            }));
            if index == 511 {
                let mut save = key(19, 0x53, true, 1);
                save.Event.KeyEvent.dwControlKeyState = 8;
                records.push(save);
                expected.push(Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)));
            }
        }
        let native_mode = ConsoleInputMode::mode(&mode.input).unwrap();
        unsafe { assert_ne!(SetConsoleMode(mode.input.as_raw_handle(), native_mode & !ENABLE_VIRTUAL_TERMINAL_INPUT), 0); }
        inject(&mode.input, &records);
        unsafe { assert_ne!(SetConsoleMode(mode.input.as_raw_handle(), native_mode), 0); }
        inject_raw_text(&mode.input, "\x1b[200~after drag 🦀\r\nsecond row\x1b[201~");
        expected.push(Event::Paste("after drag 🦀\r\nsecond row".into()));
        for event in expected { assert_eq!(next(), event); }
        assert!(!event::poll(Duration::ZERO).unwrap(), "a drained reader must be immediately idle");
        // A subsequent sparse event must wake the same persistent handle.
        inject(&mode.input, &[key(0, 0x25, true, 1)]);
        assert_eq!(next(), Event::Key(KeyCode::Left.into()));
        crossterm::terminal::disable_raw_mode().unwrap();
    }

    #[test]
    fn native_mouse_origin_can_be_fixed_and_restores_the_live_console_on_drop() {
        const PROBE: &str = "VTCODE_MOUSE_ORIGIN_TEST";
        if std::env::var_os(PROBE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows_input::tests::native_mouse_origin_can_be_fixed_and_restores_the_live_console_on_drop", "--nocapture"])
                .env(PROBE, "1").creation_flags(0x0800_0000).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        use crossterm::event::{MouseEvent, MouseEventKind};
        use windows_sys::Win32::System::Console::{CONSOLE_SCREEN_BUFFER_INFO, COORD, GetConsoleScreenBufferInfo, INPUT_RECORD_0, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED};
        let mode = ConsoleInputMode::save().unwrap();
        crossterm::terminal::enable_raw_mode().unwrap();
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        assert_ne!(unsafe { GetConsoleScreenBufferInfo(mode.output.as_raw_handle(), &mut info) }, 0);
        let top = info.srWindow.Top;
        let record = INPUT_RECORD { EventType: MOUSE_EVENT as u16, Event: INPUT_RECORD_0 { MouseEvent: MOUSE_EVENT_RECORD {
            dwMousePosition: COORD { X: 17, Y: top + 10 }, dwButtonState: 0, dwControlKeyState: 0, dwEventFlags: MOUSE_MOVED,
        } } };
        mode.use_alternate_screen_coordinates();
        for (origin, expected) in [(Some(top + 7), 3), (None, 10), (Some(top + 5), 5)] {
            crossterm::event::set_windows_mouse_viewport_origin(origin);
            inject(&mode.input, &[record]);
            assert_eq!(next(), Event::Mouse(MouseEvent { kind: MouseEventKind::Moved, column: 17, row: expected, modifiers: KeyModifiers::NONE }));
        }
        let input = mode.input.try_clone().unwrap();
        drop(mode);
        // Drop must remove the fixed origin even if callers overrode it.
        inject(&input, &[record]);
        assert_eq!(next(), Event::Mouse(MouseEvent { kind: MouseEventKind::Moved, column: 17, row: 10, modifiers: KeyModifiers::NONE }));
        crossterm::terminal::disable_raw_mode().unwrap();
    }

    #[test]
    fn alternate_screen_mouse_coordinates_ignore_primary_console_scrollback() {
        const PROBE: &str = "VTCODE_ALT_MOUSE_ORIGIN_TEST";
        if std::env::var_os(PROBE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows_input::tests::alternate_screen_mouse_coordinates_ignore_primary_console_scrollback", "--nocapture"])
                .env(PROBE, "1").creation_flags(0x0800_0000).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        use windows_sys::Win32::System::Console::{CONSOLE_SCREEN_BUFFER_INFO, COORD, FlushConsoleInputBuffer, GetConsoleScreenBufferInfo, INPUT_RECORD_0, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED, SetConsoleScreenBufferSize, SetConsoleWindowInfo, SMALL_RECT};
        let mode = ConsoleInputMode::save().unwrap();
        let handle = mode.output.as_raw_handle();
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        unsafe {
            assert_ne!(GetConsoleScreenBufferInfo(handle, &mut info), 0);
            if info.dwSize.Y < 200 {
                assert_ne!(SetConsoleScreenBufferSize(handle, COORD { X: info.dwSize.X, Y: 200 }), 0);
            }
            let window = SMALL_RECT { Left: info.srWindow.Left, Right: info.srWindow.Right,
                Top: 20, Bottom: 20 + info.srWindow.Bottom - info.srWindow.Top };
            assert_ne!(SetConsoleWindowInfo(handle, 1, &window), 0);
            assert_ne!(SetConsoleMode(handle, ConsoleInputMode::mode(&mode.output).unwrap() | ENABLE_VIRTUAL_TERMINAL_PROCESSING), 0);
        }
        let mut output = mode.output.try_clone().unwrap();
        crossterm::execute!(output, crossterm::terminal::EnterAlternateScreen).unwrap();
        mode.use_alternate_screen_coordinates();
        let active = OpenOptions::new().read(true).write(true).open("CONOUT$").unwrap();
        assert_ne!(unsafe { GetConsoleScreenBufferInfo(active.as_raw_handle(), &mut info) }, 0);
        assert_eq!(info.srWindow.Top, 0, "alternate screen has no primary scrollback origin");
        unsafe { FlushConsoleInputBuffer(mode.input.as_raw_handle()); }
        inject(&mode.input, &[INPUT_RECORD { EventType: MOUSE_EVENT as u16, Event: INPUT_RECORD_0 { MouseEvent: MOUSE_EVENT_RECORD {
            dwMousePosition: COORD { X: 17, Y: 4 }, dwButtonState: 0, dwControlKeyState: 0, dwEventFlags: MOUSE_MOVED,
        } } }]);
        let Event::Mouse(mouse) = next() else { panic!("expected alternate-screen mouse"); };
        assert_eq!((mouse.column, mouse.row), (17, 4));
        crossterm::execute!(output, crossterm::terminal::LeaveAlternateScreen).unwrap();
        let input = mode.input.try_clone().unwrap();
        drop(mode);
        unsafe { FlushConsoleInputBuffer(input.as_raw_handle()); }
        inject(&input, &[INPUT_RECORD { EventType: MOUSE_EVENT as u16, Event: INPUT_RECORD_0 { MouseEvent: MOUSE_EVENT_RECORD {
            dwMousePosition: COORD { X: 17, Y: 24 }, dwButtonState: 0, dwControlKeyState: 0, dwEventFlags: MOUSE_MOVED,
        } } }]);
        let Event::Mouse(mouse) = next() else { panic!("expected primary-screen mouse"); };
        assert_eq!((mouse.column, mouse.row), (17, 4), "restored console uses its scrolled viewport again");
    }

    #[test]
    #[ignore = "Real hidden-console input decode comparison; optimized profile, optionally under CPU contention"]
    fn benchmark_native_mouse_viewport_lookup() {
        const PROBE: &str = "VTCODE_MOUSE_DECODE_BENCH";
        if std::env::var_os(PROBE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows_input::tests::benchmark_native_mouse_viewport_lookup", "--ignored", "--nocapture"])
                .env(PROBE, "1").creation_flags(0x0800_0000).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            eprintln!("{}", String::from_utf8_lossy(&output.stderr));
            return;
        }
        use windows_sys::Win32::System::Console::{COORD, INPUT_RECORD_0, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED};
        let mode = ConsoleInputMode::save().unwrap();
        crossterm::terminal::enable_raw_mode().unwrap();
        let _priority = crate::worker_priority::InputPriority::new(true);
        let records: Vec<_> = (0..512u16).map(|index| INPUT_RECORD { EventType: MOUSE_EVENT as u16,
            Event: INPUT_RECORD_0 { MouseEvent: MOUSE_EVENT_RECORD {
                dwMousePosition: COORD { X: (index % 60) as i16, Y: (index % 20) as i16 },
                dwButtonState: 0, dwControlKeyState: 0, dwEventFlags: MOUSE_MOVED,
            } },
        }).collect();
        // Initialize the reader and reusable native buffer before measuring.
        inject(&mode.input, &records[..1]); next();
        let mut samples: [Vec<Duration>; 2] = Default::default();
        for round in 0..7 {
            for cached in [round % 2 == 0, round % 2 != 0] {
                crossterm::event::set_windows_mouse_viewport_origin(cached.then_some(0));
                inject(&mode.input, &records);
                let started = std::time::Instant::now();
                for index in 0..512u16 {
                    let Event::Mouse(mouse) = event::read().unwrap() else { panic!("expected mouse"); };
                    assert_eq!((mouse.column, mouse.row), (index % 60, index % 20));
                }
                samples[usize::from(cached)].push(started.elapsed());
            }
        }
        for sample in &mut samples { sample.sort(); }
        eprintln!("native mouse decode n=512: live viewport={:?}; fixed viewport={:?}", samples[0][3], samples[1][3]);
        crossterm::event::set_windows_mouse_viewport_origin(None);
        crossterm::terminal::disable_raw_mode().unwrap();
    }

    fn shortcut_cases() -> Vec<(u16, u16, u32, KeyEvent)> {
        let mut cases = Vec::new();
        for letter in b'A'..=b'Z' {
            cases.push((
                u16::from(letter - b'A' + 1),
                u16::from(letter),
                24,
                KeyEvent::new(
                    KeyCode::Char(char::from(letter)),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            ));
        }
        for (unit, vk, flags, code, modifiers) in [
            (112, 80, 2, KeyCode::Char('p'), KeyModifiers::ALT),
            (
                80,
                80,
                18,
                KeyCode::Char('P'),
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
            (
                16,
                80,
                10,
                KeyCode::Char('p'),
                KeyModifiers::ALT | KeyModifiers::CONTROL,
            ),
            (
                9,
                9,
                24,
                KeyCode::BackTab,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            (
                0,
                37,
                24,
                KeyCode::Left,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            (0, 116, 16, KeyCode::F(5), KeyModifiers::SHIFT),
            (127, 8, 8, KeyCode::Backspace, KeyModifiers::CONTROL),
            (8, 8, 8, KeyCode::Backspace, KeyModifiers::CONTROL),
            (8, 8, 0, KeyCode::Backspace, KeyModifiers::NONE),
            (8, 72, 8, KeyCode::Char('h'), KeyModifiers::CONTROL),
            (0, 37, 8, KeyCode::Left, KeyModifiers::CONTROL),
            (0, 39, 8, KeyCode::Right, KeyModifiers::CONTROL),
            (0, 39, 24, KeyCode::Right, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            (0, 38, 8, KeyCode::Up, KeyModifiers::CONTROL),
            (0, 40, 8, KeyCode::Down, KeyModifiers::CONTROL),
            (0, 36, 8, KeyCode::Home, KeyModifiers::CONTROL),
            (0, 35, 8, KeyCode::End, KeyModifiers::CONTROL),
            (0, 46, 8, KeyCode::Delete, KeyModifiers::CONTROL),
            (13, 13, 0, KeyCode::Enter, KeyModifiers::NONE),
            (13, 13, 16, KeyCode::Enter, KeyModifiers::SHIFT),
            (13, 13, 8, KeyCode::Enter, KeyModifiers::CONTROL),
            (
                13,
                13,
                24,
                KeyCode::Enter,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            (13, 13, 2, KeyCode::Enter, KeyModifiers::ALT),
            (
                13,
                13,
                18,
                KeyCode::Enter,
                KeyModifiers::ALT | KeyModifiers::SHIFT,
            ),
        ] {
            cases.push((unit, vk, flags, KeyEvent::new(code, modifiers)));
        }
        cases
    }

    fn enter_sequences() -> [(&'static str, KeyModifiers); 3] {
        [
            ("\x1b[13;2u", KeyModifiers::SHIFT),
            // A terminal sendInput binding for ESC + CR means Alt+Enter,
            // even when the physical shortcut assigned to it is Shift+Enter.
            ("\x1b\r", KeyModifiers::ALT),
            ("\r", KeyModifiers::NONE),
        ]
    }

    fn check_find_navigation(event: Event) {
        let Event::Key(key) = event else {
            panic!("expected a keyboard event");
        };
        crate::keyboard_protocol_tests::check_key(key);
        if key.code != KeyCode::Enter
            || !matches!(key.modifiers, KeyModifiers::NONE | KeyModifiers::SHIFT)
        {
            return;
        }
        let mut app = crate::app::App::test_new();
        app.tabs.push(crate::app::Tab::new_file(
            crate::editor::Editor::test_with_text("foo foo foo"),
        ));
        app.active_pane = crate::app::ActivePane::Editor;
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
        );
        app.search_query = "foo".into();
        app.update_search();
        assert_eq!(app.search_matches.len(), 3);
        assert_eq!(app.search_current, 0);
        let expected = if key.modifiers == KeyModifiers::SHIFT {
            [2, 1, 0, 2]
        } else {
            [1, 2, 0, 1]
        };
        for index in expected {
            assert!(!crate::input_keyboard::handle_keyboard_event(&mut app, key));
            assert_eq!(app.search_current, index, "{key:?}");
            assert_eq!(app.editor().unwrap().cursor_col, index * 4 + 3);
            assert_eq!(
                app.editor().unwrap().selection_range(),
                Some((0, index * 4, 0, index * 4 + 3))
            );
            assert!(app.search_active && app.search_input_focused);
        }
    }

    fn next_press() -> Event {
        loop {
            let event = next();
            // Match the workbench: modifier-only reports update hover state,
            // while command routing receives the actual shortcut press.
            if matches!(event, Event::Key(key) if key.kind == event::KeyEventKind::Release
                || matches!(key.code, KeyCode::Modifier(_))) {
                continue;
            }
            return event;
        }
    }

    fn check_palette_key(event: Event) {
        let Event::Key(key) = event else {
            panic!("expected command palette key");
        };
        assert_eq!(
            key,
            KeyEvent::new(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )
        );
        let mut app = crate::app::App::test_new();
        assert!(!crate::input_keyboard::handle_keyboard_event(&mut app, key));
        assert!(app.palette.active);
        assert!(!app.quick_open_active);
    }

    #[test]
    fn windows_console_paste_and_mode_restore() {
        const PROBE: &str = "VTCODE_PASTE_CONSOLE_TEST";
        if std::env::var_os(PROBE).is_none() {
            // Never inject records into the developer's interactive console.
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "windows_input::tests::windows_console_paste_and_mode_restore",
                    "--nocapture",
                ])
                .env(PROBE, "1")
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        // Exercise the non-UTF-8 defaults used by CI and restore both pages,
        // including when input and output started with different encodings.
        unsafe {
            assert_ne!(SetConsoleCP(437), 0);
            assert_ne!(SetConsoleOutputCP(850), 0);
        }
        let mode = ConsoleInputMode::save().unwrap();
        let original = mode.original;
        let original_input_code_page = mode.original_input_code_page;
        let original_output_code_page = mode.original_output_code_page;
        crossterm::terminal::enable_raw_mode().unwrap();
        crossterm::execute!(io::stdout(), event::EnableMouseCapture).unwrap();
        let mouse_mode = ConsoleInputMode::mode(&mode.input).unwrap();
        mode.enable_virtual_terminal_input().unwrap();
        unsafe {
            assert_eq!(GetConsoleCP(), UTF8_CODE_PAGE);
            assert_eq!(GetConsoleOutputCP(), UTF8_CODE_PAGE);
        }
        assert_eq!(
            ConsoleInputMode::mode(&mode.input).unwrap(),
            mouse_mode | ENABLE_VIRTUAL_TERMINAL_INPUT
        );

        // The first packet must survive even a read boundary after its ESC.
        // No earlier packet has taught the reader that W32IM is active yet.
        inject_raw_text(&mode.input, "\x1b");
        assert!(!event::poll(Duration::from_millis(150)).unwrap());
        inject_raw_text(&mode.input, "[80;25;16;1;24;1_");
        check_palette_key(next());

        check_side_button_input(&mode.input);
        check_shift_mouse_input(&mode.input);
        check_control_hover_input(&mode.input);

        let payload = "fn main() {\r\n\tprintln!(\"Tere 🦀 e\u{301}\");\r\n}\n";
        // A partial marker must survive separate reads without emitting keys.
        inject_text(&mode.input, "\x1b[20");
        assert!(!event::poll(Duration::from_millis(20)).unwrap());
        inject_text(&mode.input, &format!("0~{payload}\x1b[201~"));
        assert_eq!(next(), Event::Paste(payload.into()));

        // Repeated characters, control characters and release records remain literal.
        inject_text(&mode.input, "\x1b[200~");
        inject(&mode.input, &[key('x' as u16, 0, true, 3)]);
        inject(&mode.input, &[key('x' as u16, 0, false, 1)]);
        inject_text(&mode.input, "\x03\x1b[20z\x1b[201~\x1b[200~\x1b[201~");
        assert_eq!(next(), Event::Paste("xxx\x03\x1b[20z".into()));
        assert_eq!(next(), Event::Paste(String::new()));

        // Older ConPTY synthesizes Alt+numpad input for characters without a
        // keyboard mapping. The UTF-16 unit arrives on Alt release, including
        // each half of an emoji. Exercise that path even on newer consoles.
        for encoded in [false, true] {
            inject_raw_text(&mode.input, "\x1b[200~Tere ");
            let input_mode = ConsoleInputMode::mode(&mode.input).unwrap();
            if !encoded {
                unsafe {
                    assert_ne!(
                        SetConsoleMode(
                            mode.input.as_raw_handle(),
                            input_mode & !ENABLE_VIRTUAL_TERMINAL_INPUT,
                        ),
                        0
                    );
                }
            }
            for unit in "é 🦀".encode_utf16() {
                inject(
                    &mode.input,
                    &[
                        key(0, 0x12, true, 1),
                        key(0, 0x60, true, 1),
                        key(0, 0x60, false, 1),
                        key(unit, 0x12, false, 1),
                    ],
                );
            }
            if !encoded {
                unsafe {
                    assert_ne!(SetConsoleMode(mode.input.as_raw_handle(), input_mode), 0);
                }
            }
            inject_raw_text(&mode.input, "\x1b[201~");
            assert_eq!(
                next(),
                Event::Paste("Tere é 🦀".into()),
                "encoded: {encoded}"
            );
        }

        // Native pasted text can contain protocol-looking characters. Decode
        // only the outer transport packets and keep this payload literal.
        inject_text(
            &mode.input,
            "\x1b[200~literal \x1b[80;25;16;1;24;1_ text\x1b[201~",
        );
        assert_eq!(
            next(),
            Event::Paste("literal \x1b[80;25;16;1;24;1_ text".into())
        );

        // Windows Terminal mixes raw paste delimiters/text with W32IM control
        // keys. Reproduce the reported Enter press/release leaking into text.
        inject_raw_text(&mode.input, "\x1b[200~rsists across tab switches).");
        let mut enter = key(13, 13, true, 1);
        enter.Event.KeyEvent.wVirtualScanCode = 28;
        let mut release = enter;
        release.Event.KeyEvent.bKeyDown = 0;
        inject(&mode.input, &[enter, release]);
        inject_raw_text(&mode.input, "- [x] Fix exit animation us\x1b[201~");
        assert_eq!(
            next(),
            Event::Paste("rsists across tab switches).\r- [x] Fix exit animation us".into())
        );

        // Exercise every raw/encoded combination of delimiter, body and end,
        // with transitions around UTF-16 surrogate pairs and control keys.
        for formats in 0..8 {
            let begin: fn(&File, &str) = if formats & 1 == 0 {
                inject_raw_text
            } else {
                inject_text
            };
            let body: fn(&File, &str) = if formats & 2 == 0 {
                inject_raw_text
            } else {
                inject_text
            };
            let end: fn(&File, &str) = if formats & 4 == 0 {
                inject_raw_text
            } else {
                inject_text
            };
            begin(&mode.input, "\x1b[200~first 🦀");
            inject(&mode.input, &[enter, release]);
            body(&mode.input, "\nsecond é");
            inject(&mode.input, &[key(9, 9, true, 1), key(9, 9, false, 1)]);
            end(&mode.input, "end\x1b[201~");
            assert_eq!(
                next(),
                Event::Paste("first 🦀\r\nsecond é\tend".into()),
                "format combination {formats}"
            );
        }

        // Escape must still work if an end marker is lost.
        inject_raw_text(&mode.input, "\x1b[200~unfinished");
        assert!(!event::poll(Duration::from_millis(20)).unwrap());
        inject(&mode.input, &[key(27, 27, true, 1)]);
        assert!(matches!(next(), Event::PasteStatus(event::PasteStatus::Failed {
            reason: event::PasteFailure::Cancelled, ..
        })));
        inject_raw_text(&mode.input, "\x03\x11\x1b[201~\x1b[200~recovered\x1b[201~");
        assert_eq!(next(), Event::Paste("recovered".into()));

        // A network or CPU stall must not turn the remainder into shortcuts.
        inject_raw_text(&mode.input, "\x1b[200~unfinished");
        assert!(!event::poll(Duration::from_millis(2100)).unwrap());
        inject_raw_text(&mode.input, " after pause\x1b[201~");
        assert_eq!(next(), Event::Paste("unfinished after pause".into()));

        // Invalid UTF-16 must preserve the valid unit that follows it.
        inject(
            &mode.input,
            &[key(0xD800, 0, true, 1), key('x' as u16, 0, true, 1)],
        );
        assert_eq!(next(), Event::Key(KeyCode::Char('\u{fffd}').into()));
        assert_eq!(next(), Event::Key(KeyCode::Char('x').into()));

        // Escape, arrows and Ctrl+S still use the upstream native key parser.
        let mut save = key(19, 0x53, true, 1);
        save.Event.KeyEvent.dwControlKeyState = 0x0008;
        inject(
            &mode.input,
            &[key(27, 0x1B, true, 1), key(0, 0x25, true, 1), save],
        );
        assert_eq!(
            next(),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        );
        assert_eq!(
            next(),
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
        );
        assert_eq!(
            next(),
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        );

        let mut palette = key(16, 0x50, true, 1);
        palette.Event.KeyEvent.dwControlKeyState = 0x0010 | 0x0008;
        inject(&mode.input, &[palette]);
        check_palette_key(next());
        for (unit, vk, flags, expected) in shortcut_cases() {
            let mut record = key(unit, vk, true, 1);
            record.Event.KeyEvent.dwControlKeyState = flags;
            inject(&mode.input, &[record]);
            let actual = next();
            assert_eq!(
                actual,
                Event::Key(expected),
                "virtual key {vk}, flags {flags}"
            );
            check_find_navigation(actual);
        }
        for (sequence, modifiers) in enter_sequences() {
            inject_raw_text(&mode.input, sequence);
            let actual = next();
            assert_eq!(
                actual,
                Event::Key(KeyEvent::new(KeyCode::Enter, modifiers)),
                "{sequence:?}"
            );
            check_find_navigation(actual);
        }
        // Model an SSH/VT sequence split at every read boundary, including
        // immediately after Escape. Poll between fragments as the UI does.
        for (sequence, expected) in crate::keyboard_protocol_tests::fragmented_shortcut_sequences() {
            for split in 1..sequence.len() {
                inject_raw_text(&mode.input, &sequence[..split]);
                assert!(!event::poll(Duration::from_millis(5)).unwrap(), "premature key at {sequence:?}, split {split}");
                inject_raw_text(&mode.input, &sequence[split..]);
                let actual = next();
                assert_eq!(actual, Event::Key(expected), "{sequence:?}, split {split}");
                crate::keyboard_protocol_tests::check_key(expected);
            }
        }
        for (sequence, expected) in crate::keyboard_protocol_tests::vt_cases() {
            inject_raw_text(&mode.input, sequence);
            let actual = next();
            assert_eq!(actual, Event::Key(expected), "{sequence:?}");
            crate::keyboard_protocol_tests::check_key(expected);
        }
        let mut release = key(16, 80, false, 1);
        release.Event.KeyEvent.dwControlKeyState = 24;
        inject(&mode.input, &[release]);
        assert_eq!(
            next(),
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('P'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                event::KeyEventKind::Release
            ))
        );

        crossterm::terminal::disable_raw_mode().unwrap();
        crossterm::execute!(io::stdout(), event::DisableMouseCapture).unwrap();
        let input = mode.input.try_clone().unwrap();
        let output = mode.output.try_clone().unwrap();
        let original_output = mode.original_output;
        drop(mode);
        assert_eq!(ConsoleInputMode::mode(&input).unwrap(), original);
        assert_eq!(ConsoleInputMode::mode(&output).unwrap(), original_output);
        unsafe {
            assert_eq!(GetConsoleCP(), original_input_code_page);
            assert_eq!(GetConsoleOutputCP(), original_output_code_page);
        }

        // Restoration must also run if terminal initialization unwinds.
        let _ = std::panic::catch_unwind(|| {
            let mode = ConsoleInputMode::save().unwrap();
            mode.enable_virtual_terminal_input().unwrap();
            panic!("simulated setup failure");
        });
        assert_eq!(ConsoleInputMode::mode(&input).unwrap(), original);
        assert_eq!(ConsoleInputMode::mode(&output).unwrap(), original_output);
        unsafe {
            assert_eq!(GetConsoleCP(), original_input_code_page);
            assert_eq!(GetConsoleOutputCP(), original_output_code_page);
        }
    }

    #[test]
    fn windows_console_rejects_invalid_cursor_reports_and_keeps_extended_keys() {
        const PROBE: &str = "VTCODE_NATIVE_DECODER_CONSOLE";
        const TEST: &str = "windows_input::tests::windows_console_rejects_invalid_cursor_reports_and_keeps_extended_keys";
        if std::env::var_os(PROBE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"]).env(PROBE, "1")
                .creation_flags(0x0800_0000).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        let mode = ConsoleInputMode::save().unwrap();
        crossterm::terminal::enable_raw_mode().unwrap();
        mode.prepare_bridge().unwrap();
        inject_raw_text(&mode.input, "\x1b[0;0R\x1b[1;0R\x1b[65536;1R");
        for (bytes, expected) in crate::keyboard_protocol_tests::extended_key_cases() {
            inject_raw_text(&mode.input, &bytes);
            assert_eq!(next(), Event::Key(expected), "bridge key {bytes:?}");
        }
    }

    #[test]
    fn windows_console_million_line_paste_preserves_unicode_progress_and_following_keys() {
        const PROBE: &str = "VTCODE_MILLION_LINE_PASTE_TEST";
        const LINE: &str = "row é🦀0123456789\n";
        if std::env::var_os(PROBE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "windows_input::tests::windows_console_million_line_paste_preserves_unicode_progress_and_following_keys", "--nocapture"])
                .env(PROBE, "1").creation_flags(0x0800_0000).output().unwrap();
            assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        let mode = ConsoleInputMode::save().unwrap();
        crossterm::terminal::enable_raw_mode().unwrap();
        crossterm::execute!(io::stdout(), event::EnableMouseCapture).unwrap();
        mode.enable_virtual_terminal_input().unwrap();
        let input = mode.input.try_clone().unwrap();
        let producer = std::thread::spawn(move || {
            inject_raw_text(&input, "\x1b[200~");
            let chunk = LINE.repeat(4096);
            for index in 0..1_000_000 / 4096 + 1 {
                let deadline = std::time::Instant::now() + Duration::from_secs(30);
                loop {
                    let mut queued = 0;
                    assert_ne!(unsafe { windows_sys::Win32::System::Console::GetNumberOfConsoleInputEvents(input.as_raw_handle(), &mut queued) }, 0);
                    if queued <= 65_536 { break; }
                    assert!(std::time::Instant::now() < deadline, "bulk input stopped draining");
                    std::thread::sleep(Duration::from_millis(1));
                }
                if index == 1_000_000 / 4096 {
                    inject_raw_text(&input, &LINE.repeat(1_000_000 % 4096));
                } else { inject_raw_text(&input, &chunk); }
            }
            inject_raw_text(&input, "\x1b[201~");
        });
        let mut received = 0;
        loop {
            assert!(event::poll(Duration::from_secs(30)).unwrap(), "paste stalled after {received} bytes");
            match event::read().unwrap() {
                Event::PasteStatus(event::PasteStatus::Receiving { bytes }) => { assert!(bytes > received); received = bytes; }
                Event::Paste(text) => {
                    assert!(received > 10 * 1024 * 1024);
                    assert_eq!(text.len(), LINE.len() * 1_000_000);
                    assert!(text.as_bytes().chunks(LINE.len()).all(|row| row == LINE.as_bytes()));
                    break;
                }
                other => panic!("bulk text became {other:?}"),
            }
        }
        producer.join().unwrap();
        inject_raw_text(&mode.input, "\x1b[80;25;16;1;24;1_");
        check_palette_key(next_press());
        assert!(!event::poll(Duration::ZERO).unwrap());
    }

    #[test]
    fn windows_conpty_delivers_bracketed_paste() {
        if let Ok(value) = std::env::var("VTCODE_PASTE_CONPTY_TEST") {
            conpty_paste_probe(value == "1");
        } else {
            conpty_paste_probe(false);
            conpty_paste_probe(true);
        }
    }

    fn conpty_paste_probe(enable_vt: bool) {
        use std::io::{Read, Write};
        const PROBE: &str = "VTCODE_PASTE_CONPTY_TEST";
        const READY: &str = "VTCODE_PASTE_READY";
        const PAYLOAD: &str = "if (true) {\r\n\tTere 🦀\r\n}\n";
        if std::env::var_os(PROBE).is_some() {
            unsafe {
                assert_ne!(SetConsoleCP(437), 0);
                assert_ne!(SetConsoleOutputCP(850), 0);
            }
            let mode = ConsoleInputMode::save().unwrap();
            crossterm::terminal::enable_raw_mode().unwrap();
            crossterm::execute!(
                io::stdout(),
                event::EnableMouseCapture,
                event::EnableBracketedPaste
            )
            .unwrap();
            if enable_vt {
                mode.enable_virtual_terminal_input().unwrap();
            }
            println!("{READY}");
            io::stdout().flush().unwrap();
            if enable_vt {
                assert_eq!(next_press(), Event::Paste(PAYLOAD.into()));
                assert_eq!(
                    next_press(),
                    Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
                );
                assert_eq!(
                    next_press(),
                    Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
                );
                check_palette_key(next_press());
                println!("VTCODE_SHORTCUT_READY");
                io::stdout().flush().unwrap();
                for (index, (_, _, _, expected)) in shortcut_cases().into_iter().enumerate() {
                    let actual = next_press();
                    assert_eq!(actual, Event::Key(expected));
                    check_find_navigation(actual);
                    println!("VTCODE_SHORTCUT_{index}_OK");
                    io::stdout().flush().unwrap();
                }
                for (index, (sequence, modifiers)) in enter_sequences().into_iter().enumerate() {
                    let actual = next_press();
                    assert_eq!(
                        actual,
                        Event::Key(KeyEvent::new(KeyCode::Enter, modifiers)),
                        "{sequence:?}"
                    );
                    check_find_navigation(actual);
                    println!("VTCODE_ENTER_{index}_OK");
                    io::stdout().flush().unwrap();
                }
                assert_eq!(next_press(), Event::Paste(PAYLOAD.repeat(512)));
            } else {
                assert_eq!(
                    next(),
                    Event::Key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE))
                );
            }
            crossterm::execute!(
                io::stdout(),
                event::DisableBracketedPaste,
                event::DisableMouseCapture
            )
            .unwrap();
            return;
        }

        // Feed the same VT input bytes as Windows Terminal through real ConPTY,
        // so the OS (rather than our test) creates the native input records.
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "windows_input::tests::windows_conpty_delivers_bracketed_paste",
            "--nocapture",
        ]);
        command.env(PROBE, if enable_vt { "1" } else { "0" });
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(n) = reader.read(&mut bytes) {
                if n == 0 || tx.send(bytes[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains(READY) {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(bytes) => output.extend(bytes),
                Err(error) => {
                    let _ = child.kill();
                    panic!(
                        "ConPTY did not become ready: {error}: {}",
                        String::from_utf8_lossy(&output)
                    );
                }
            }
        }
        writer
            .write_all(format!("\x1b[200~{PAYLOAD}\x1b[201~\x1b[D\x13").as_bytes())
            .unwrap();
        writer.flush().unwrap();
        if enable_vt {
            // WezTerm and Windows Terminal send physical keys to ConPTY using
            // Win32 Input Mode. Include Ctrl+Shift+P rather than just Ctrl+P's
            // ambiguous control byte, which was all the earlier test exercised.
            writer.write_all(b"\x1b[80;25;16;1;24;1_").unwrap();
            let mut wait_for = |marker: &str| {
                while !String::from_utf8_lossy(&output).contains(marker) {
                    match rx.recv_timeout(Duration::from_secs(3)) {
                        Ok(bytes) => output.extend(bytes),
                        Err(error) => {
                            let _ = child.kill();
                            panic!(
                                "ConPTY did not report {marker}: {error}: {}",
                                String::from_utf8_lossy(&output)
                            );
                        }
                    }
                }
            };
            wait_for("VTCODE_SHORTCUT_READY");
            // Model separate physical keys. ConPTY's input parser can flush an
            // incomplete Win32 packet at a pipe-read boundary during a burst.
            for (index, (unit, vk, flags, _)) in shortcut_cases().into_iter().enumerate() {
                writer
                    .write_all(format!("\x1b[{vk};0;{unit};1;{flags};1_").as_bytes())
                    .unwrap();
                writer.flush().unwrap();
                wait_for(&format!("VTCODE_SHORTCUT_{index}_OK"));
            }
            for (index, (sequence, _)) in enter_sequences().into_iter().enumerate() {
                writer.write_all(sequence.as_bytes()).unwrap();
                writer.flush().unwrap();
                wait_for(&format!("VTCODE_ENTER_{index}_OK"));
            }
            writer
                .write_all(format!("\x1b[200~{}\x1b[201~", PAYLOAD.repeat(512)).as_bytes())
                .unwrap();
            writer.flush().unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                panic!("ConPTY paste test timed out");
            }
            if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(20)) {
                output.extend(bytes);
            }
        };
        while let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
            output.extend(bytes);
        }
        assert!(
            status.success(),
            "ConPTY paste failed: {}",
            String::from_utf8_lossy(&output)
        );
    }
}
