//! Opt-in end-to-end probe: real ConPTY input, reader, dispatch and ANSI output.
//! Unlike an injected event-queue benchmark this includes the console host.
use super::*;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

const CHILD: &str = "VTCODE_POINTER_PIPELINE_CHILD";
const TEST: &str =
    "app_event_loop::windows_pointer_pipeline_tests::benchmark_windows_console_pointer_pipeline";
const ACK: &[u8] = b"VTCODE_POINTER_ACK";

// An optional native-record fixture isolates console-host scheduling from
// ConPTY's VT packet parser. It attaches only to the private child created by
// this probe, and requires redirected output so no user console is modified.
pub(super) struct NativeInput(std::fs::File);
impl NativeInput {
    pub(super) fn attach(pid: u32) -> Self {
        use std::{io::IsTerminal, os::windows::io::AsRawHandle};
        use windows_sys::Win32::System::Console::{
            AttachConsole, ENABLE_VIRTUAL_TERMINAL_INPUT, FreeConsole, GetConsoleMode,
            SetConsoleMode,
        };
        assert!(
            !std::io::stdout().is_terminal(),
            "redirect the native probe's output"
        );
        unsafe {
            FreeConsole();
            assert_ne!(AttachConsole(pid), 0, "private console attach failed");
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONIN$")
            .unwrap();
        let mut mode = 0;
        unsafe {
            assert_ne!(GetConsoleMode(file.as_raw_handle(), &mut mode), 0);
            assert_ne!(
                SetConsoleMode(file.as_raw_handle(), mode & !ENABLE_VIRTUAL_TERMINAL_INPUT),
                0
            );
        }
        Self(file)
    }

    pub(super) fn record(
        column: u16,
        row: u16,
        moved: bool,
        up: bool,
    ) -> windows_sys::Win32::System::Console::INPUT_RECORD {
        use windows_sys::Win32::System::Console::{
            COORD, INPUT_RECORD, INPUT_RECORD_0, MOUSE_EVENT, MOUSE_EVENT_RECORD, MOUSE_MOVED,
        };
        INPUT_RECORD {
            EventType: MOUSE_EVENT as u16,
            Event: INPUT_RECORD_0 {
                MouseEvent: MOUSE_EVENT_RECORD {
                    dwMousePosition: COORD {
                        X: column as i16,
                        Y: row as i16,
                    },
                    dwButtonState: u32::from(!up),
                    dwControlKeyState: 0,
                    dwEventFlags: if moved { MOUSE_MOVED } else { 0 },
                },
            },
        }
    }

    pub(super) fn send(&self, records: &[windows_sys::Win32::System::Console::INPUT_RECORD]) {
        use std::os::windows::io::AsRawHandle;
        let mut written = 0;
        assert_ne!(
            unsafe {
                windows_sys::Win32::System::Console::WriteConsoleInputW(
                    self.0.as_raw_handle(),
                    records.as_ptr(),
                    records.len() as u32,
                    &mut written,
                )
            },
            0
        );
        assert_eq!(written as usize, records.len());
    }

    pub(super) fn stream_send(&self, records: &[windows_sys::Win32::System::Console::INPUT_RECORD]) {
        static RAW: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*RAW.get_or_init(|| std::env::var_os("VTCODE_STREAM_RAW").is_some()) {
            self.send(records);
            return;
        }
        use windows_sys::Win32::System::Console::{INPUT_RECORD, INPUT_RECORD_0, KEY_EVENT, KEY_EVENT_RECORD, KEY_EVENT_RECORD_0, MOUSE_EVENT, MOUSE_MOVED};
        let mut encoded = Vec::new();
        for record in records {
            assert_eq!(record.EventType, MOUSE_EVENT as u16);
            // SAFETY: the checked tag identifies the active union member.
            let mouse = unsafe { record.Event.MouseEvent };
            let code = if mouse.dwEventFlags == MOUSE_MOVED { 32 } else { 0 };
            let text = format!("\x1b[<{code};{};{}{}", mouse.dwMousePosition.X + 1, mouse.dwMousePosition.Y + 1, if mouse.dwButtonState == 0 { 'm' } else { 'M' });
            encoded.extend(text.encode_utf16().map(|unit| INPUT_RECORD {
                EventType: KEY_EVENT as u16,
                Event: INPUT_RECORD_0 { KeyEvent: KEY_EVENT_RECORD {
                    bKeyDown: 1, wRepeatCount: 1, wVirtualKeyCode: 0, wVirtualScanCode: 0,
                    uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit }, dwControlKeyState: 0,
                } },
            }));
        }
        self.send(&encoded);
    }
}
impl Drop for NativeInput {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Console::FreeConsole();
        }
    }
}

#[test]
#[ignore = "Real isolated ConPTY latency probe; run optimized, optionally pinned with competing CPU load"]
fn benchmark_windows_console_pointer_pipeline() {
    if std::env::var_os(CHILD).is_some() {
        child();
        return;
    }
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 50,
            cols: 160,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        TEST,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    command.env(CHILD, "1");
    // This is a private ConPTY, not a real foreground Windows Terminal pane.
    command.env("TERM_PROGRAM", "vtcode");
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    struct Stop(Box<dyn portable_pty::Child + Send + Sync>);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }
    let mut child = Stop(child);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reading = std::thread::spawn(move || {
        let mut bytes = [0; 65536];
        while let Ok(len) = reader.read(&mut bytes) {
            if len == 0 || tx.send(bytes[..len].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut output = Vec::new();
    let mut received = 0usize;
    let mut wait = |marker: &[u8]| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(index) = output
                .windows(marker.len())
                .position(|bytes| bytes == marker)
            {
                output.drain(..index + marker.len());
                return;
            }
            let bytes = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|_| {
                    panic!(
                        "console probe timeout: {}",
                        String::from_utf8_lossy(&output)
                    )
                });
            received += bytes.len();
            output.extend(bytes);
        }
    };
    wait(b"VTCODE_POINTER_READY");
    let native = std::env::var_os("VTCODE_POINTER_NATIVE")
        .map(|_| NativeInput::attach(child.0.process_id().unwrap()));
    // A fresh ConPTY must first see a Win32 input record, as it would from
    // Windows Terminal's keyboard. Otherwise it synthesizes physical keys
    // for the raw escape sequence instead of passing through VT mouse input.
    if let Some(native) = &native {
        native.send(&[NativeInput::record(44, 2, false, false)]);
    } else {
        writer
            .write_all(b"\x1b[16;42;0;0;0;1_\x1b[<0;45;3M")
            .unwrap();
    }
    writer.flush().unwrap();
    wait(ACK);
    let mut samples = Vec::new();
    for index in 0..120u16 {
        let started = Instant::now();
        if let Some(native) = &native {
            native.send(&[NativeInput::record(
                49 + index * 13 % 60,
                4 + index * 7 % 35,
                true,
                false,
            )]);
        } else {
            writer
                .write_all(
                    format!("\x1b[<32;{};{}M", 50 + index * 13 % 60, 5 + index * 7 % 35).as_bytes(),
                )
                .unwrap();
        }
        writer.flush().unwrap();
        wait(ACK);
        samples.push(started.elapsed());
    }
    // Up is an ordering boundary and must follow the final selected endpoint.
    let started = Instant::now();
    let mut burst = String::new();
    for index in 0..512u16 {
        use std::fmt::Write;
        write!(
            burst,
            "\x1b[<32;{};{}M",
            50 + index * 13 % 60,
            5 + index * 7 % 35
        )
        .unwrap();
    }
    burst.push_str("\x1b[<0;110;40m");
    if let Some(native) = &native {
        let mut records: Vec<_> = (0..512u16)
            .map(|index| NativeInput::record(49 + index * 13 % 60, 4 + index * 7 % 35, true, false))
            .collect();
        records.push(NativeInput::record(109, 39, false, true));
        native.send(&records);
    } else {
        writer.write_all(burst.as_bytes()).unwrap();
    }
    writer.flush().unwrap();
    wait(b"VTCODE_POINTER_DONE");
    let burst = started.elapsed();
    drop(wait);
    samples.sort();
    eprintln!(
        "ConPTY input-to-output n=120 p50={:?} p95={:?} p99={:?}; burst512={burst:?}; output={received} bytes",
        samples[60], samples[114], samples[118]
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "console probe did not exit");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(native);
    drop(writer);
    drop(pair.master);
    reading.join().unwrap();
}

fn child() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::backend::CrosstermBackend;
    let mode = crate::windows_input::ConsoleInputMode::save().unwrap();
    crossterm::terminal::enable_raw_mode().unwrap();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )
    .unwrap();
    mode.use_alternate_screen_coordinates();
    mode.enable_virtual_terminal_input().unwrap();
    let mut app = super::responsiveness_tests::fixture(1);
    let mut terminal = crate::terminal_output::event_driven_terminal(CrosstermBackend::new(
        crate::terminal_output::buffered_output(std::io::stdout()),
    ))
    .unwrap();
    for _ in 0..3 {
        refresh_caches(&mut app);
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.background.active_job_count() == 0
        });
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    }
    let wake = app.background.wake_handle();
    let mut pump = TerminalEventPump::start(wake.clone()).unwrap();
    let code = app.code_area.unwrap();
    mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: code.x + 2,
            row: code.y + 1,
            modifiers: KeyModifiers::NONE,
        },
    );
    terminal
        .backend_mut()
        .write_all(b"\x1b]0;VTCODE_POINTER_READY\x07")
        .unwrap();
    terminal.backend_mut().flush().unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut sample = 0usize;
    loop {
        let generation = wake.generation();
        let Some(event) = pump.try_recv().unwrap() else {
            assert!(Instant::now() < deadline, "no input from ConPTY");
            wake.wait_for_change(generation, Some(Duration::from_secs(1)));
            continue;
        };
        let Event::Mouse(event) = event else {
            eprintln!("Non-pointer input: {event:?}");
            continue;
        };
        let done = matches!(event.kind, MouseEventKind::Up(_));
        // Match the main loop's dispatch-through-presentation scheduling scope.
        let _frame_priority = crate::worker_priority::InputPriority::new(true);
        if handle_mouse_input(&mut app, &mut pump, event) {
            app.sync_duplicate_file_tabs();
            app.ensure_active_tab_visible();
            refresh_caches(&mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        }
        sample += 1;
        // ConPTY deliberately omits repeated title assignments. A unique
        // sequence makes every acknowledgement observable by the parent.
        write!(
            terminal.backend_mut(),
            "\x1b]0;VTCODE_POINTER_{}{sample}\x07",
            if done { "DONE" } else { "ACK" }
        )
        .unwrap();
        terminal.backend_mut().flush().unwrap();
        if done {
            break;
        }
    }
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )
    .unwrap();
    crossterm::terminal::disable_raw_mode().unwrap();
}
