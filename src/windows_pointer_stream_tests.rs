//! Native continuous input and actual event-loop output through a private ConPTY.
use super::windows_pointer_pipeline_tests::NativeInput;
use super::*;
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime},
};

const CHILD: &str = "VTCODE_POINTER_STREAM_CHILD";
const TEST: &str =
    "app_event_loop::windows_pointer_stream_tests::benchmark_windows_continuous_pointer_stream";
const PREFIX: &[u8] = b"VTCODE_STREAM_";
const N: usize = 600;

/// The system ConPTY can be much older than Windows Terminal's private host.
/// Allow the ignored probe to use an explicitly supplied Microsoft test SDK.
/// No runtime is downloaded, installed or selected by normal editor startup.
struct ConsoleHostModule(windows_sys::Win32::Foundation::HMODULE);
impl ConsoleHostModule {
    fn load() -> Option<Self> {
        use std::os::windows::ffi::OsStrExt;
        let path = std::env::var_os("VTCODE_POINTER_CONPTY_DLL")?;
        assert!(
            std::path::Path::new(&path).is_absolute(),
            "use an absolute SDK DLL path"
        );
        let path: Vec<_> = path.encode_wide().chain(Some(0)).collect();
        // SAFETY: the explicit, NUL-terminated test-runtime path is live.
        let module =
            unsafe { windows_sys::Win32::System::LibraryLoader::LoadLibraryW(path.as_ptr()) };
        assert!(
            !module.is_null(),
            "test console runtime: {}",
            io::Error::last_os_error()
        );
        Some(Self(module))
    }
}
impl Drop for ConsoleHostModule {
    fn drop(&mut self) {
        // SAFETY: load acquired this reference; portable-pty retains its own
        // library reference after selecting its console implementation.
        unsafe {
            windows_sys::Win32::Foundation::FreeLibrary(self.0);
        }
    }
}

#[test]
#[ignore = "Real continuous native mouse input through the complete loop and ConPTY; redirect stdout"]
fn benchmark_windows_continuous_pointer_stream() {
    if std::env::var_os(CHILD).is_some() {
        child();
        return;
    }
    let _runtime = ConsoleHostModule::load();
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
    let (tx, rx) = std::sync::mpsc::channel();
    let reading = std::thread::spawn(move || {
        let mut bytes = [0; 65536];
        while let Ok(len) = reader.read(&mut bytes) {
            if len == 0
                || tx
                    .send((Instant::now(), SystemTime::now(), bytes[..len].to_vec()))
                    .is_err()
            {
                break;
            }
        }
    });
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, _, bytes) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        output.extend(bytes);
        if output
            .windows(20)
            .any(|window| window == b"VTCODE_STREAM_READY\x07")
        {
            break;
        }
    }
    let mut screen = crate::terminal::TermGrid::new(160, 50);
    let mut parser = vte::Parser::new();
    for &byte in &output {
        parser.advance(&mut screen, byte);
    }
    output.clear();
    let native = Arc::new(NativeInput::attach(child.0.process_id().unwrap()));
    let input = native.clone();
    let times = Arc::new(Mutex::new(Vec::<(usize, Instant, SystemTime)>::new()));
    let sent = times.clone();
    let producer = std::thread::spawn(move || {
        input.stream_send(&[NativeInput::record(49, 4, false, false)]);
        for index in 0..N {
            let point = index * 37 % 2400;
            sent.lock()
                .unwrap()
                .push((point, Instant::now(), SystemTime::now()));
            input.stream_send(&[NativeInput::record(
                49 + (point % 80) as u16,
                4 + (point / 80) as u16,
                true,
                false,
            )]);
            if std::env::var_os("VTCODE_STREAM_BURST").is_none() {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let point = (N - 1) * 37 % 2400;
        input.stream_send(&[NativeInput::record(
            49 + (point % 80) as u16,
            4 + (point / 80) as u16,
            false,
            true,
        )]);
    });
    let mut ages = Vec::new();
    let mut state_ages = Vec::new();
    let mut render_ages = Vec::new();
    let mut delivery_ages = Vec::new();
    let mut latest_point = None;
    let mut frame_count = 0;
    let mut bytes_read = 0;
    let mut done = false;
    let mut last_screen_point = None;
    let mut screen_ages = Vec::new();
    let mut screen_intervals = Vec::new();
    let mut last_screen_at = None;
    let final_point = (N - 1) * 37 % 2400;
    while !done || last_screen_point != Some(final_point) {
        let (at, wall_at, bytes) = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| {
                panic!(
                    "stream timed out after {} inputs: {}",
                    times.lock().unwrap().len(),
                    String::from_utf8_lossy(&output)
                )
            });
        bytes_read += bytes.len();
        // Parse real painted cells separately from OSC title acknowledgements.
        // ConPTY can deliver those controls before its screen renderer runs.
        for &byte in &bytes {
            parser.advance(&mut screen, byte);
            if let Some(point) = painted_point(&screen)
                && last_screen_point != Some(point)
            {
                last_screen_point = Some(point);
                if let Some(&(_, sent, _)) = times
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|&&(candidate, _, _)| candidate == point)
                {
                    screen_ages.push(at.saturating_duration_since(sent));
                    if let Some(previous) = last_screen_at {
                        screen_intervals.push(at.saturating_duration_since(previous));
                    }
                    last_screen_at = Some(at);
                }
            }
        }
        output.extend(bytes);
        loop {
            let Some(start) = output
                .windows(PREFIX.len())
                .position(|window| window == PREFIX)
            else {
                break;
            };
            let Some(end) = output[start..].iter().position(|&byte| byte == 7) else {
                break;
            };
            let payload = String::from_utf8_lossy(&output[start + PREFIX.len()..start + end]);
            if payload == "DONE" {
                done = true;
            }
            let fields: Vec<_> = payload.split('_').collect();
            if let Some(point) = fields.first().and_then(|value| value.parse::<usize>().ok())
                && latest_point != Some(point)
            {
                latest_point = Some(point);
                let times = times.lock().unwrap();
                if let Some(&(_, sent, wall_sent)) =
                    times.iter().find(|&&(candidate, _, _)| candidate == point)
                {
                    ages.push(at.saturating_duration_since(sent));
                    if let Some(count) = fields.get(1).and_then(|value| value.parse::<usize>().ok())
                    {
                        frame_count = frame_count.max(count);
                    }
                    if let (Some(state), Some(render)) = (
                        fields.get(2).and_then(|value| value.parse::<u64>().ok()),
                        fields.get(3).and_then(|value| value.parse::<u64>().ok()),
                    ) {
                        let wall_sent = wall_sent
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos() as u64;
                        let wall_at = wall_at
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos() as u64;
                        state_ages.push(Duration::from_nanos(state.saturating_sub(wall_sent)));
                        render_ages.push(Duration::from_nanos(render.saturating_sub(wall_sent)));
                        delivery_ages.push(Duration::from_nanos(wall_at.saturating_sub(render)));
                    }
                }
            }
            output.drain(..start + end + 1);
        }
        // Keep only a possible marker prefix, never the captured screen history.
        if output.len() > 65536 {
            output.drain(..output.len() - 128);
        }
    }
    producer.join().unwrap();
    let mut status = None;
    while status.is_none() {
        status = child.0.try_wait().unwrap();
        assert!(Instant::now() < deadline, "stream child did not exit");
        if status.is_none() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(status.unwrap().success());
    drop(native);
    drop(pair.master);
    reading.join().unwrap();
    ages.sort();
    assert!(!ages.is_empty(), "missing output samples");
    eprintln!(
        "native stream input={N} frames~{frame_count} observed={} output={bytes_read} bytes: p50={:?} p95={:?} p99={:?}",
        ages.len(),
        ages[ages.len() / 2],
        ages[ages.len() * 95 / 100],
        ages[ages.len() * 99 / 100]
    );
    for (name, mut samples) in [
        ("state", state_ages),
        ("render", render_ages),
        ("terminal delivery", delivery_ages),
        ("painted screen delivery", screen_ages),
        ("screen update interval", screen_intervals),
    ] {
        if !samples.is_empty() {
            samples.sort();
            eprintln!(
                "{name}: p50={:?} p95={:?} p99={:?}",
                samples[samples.len() / 2],
                samples[samples.len() * 95 / 100],
                samples[samples.len() * 99 / 100]
            );
        }
    }
}

fn painted_point(screen: &crate::terminal::TermGrid) -> Option<usize> {
    let row = &screen.cells[0];
    if row[0].ch != '@' || row[9].ch != '@' {
        return None;
    }
    let hex = |mut range: std::ops::Range<usize>| {
        range.try_fold(0u16, |n, i| Some((n << 4) | row[i].ch.to_digit(16)? as u16))
    };
    let point = hex(1..5)?;
    (point == !hex(5..9)? && point < 2400).then_some(usize::from(point))
}

fn child() {
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
    app.source_control_enabled = false;
    let mut terminal = crate::terminal_output::event_driven_terminal(CrosstermBackend::new(
        crate::terminal_output::buffered_output(Box::new(std::io::stdout()) as Box<dyn io::Write>),
    ))
    .unwrap();
    let pump = TerminalEventPump::start(app.background.wake_handle()).unwrap();
    let mut ready = false;
    let mut frame = 0;
    super::pointer_event_loop_latency_tests::enable_screen_marker();
    super::pointer_event_loop_latency_tests::observe(move |app| {
        frame += 1;
        let mut out = std::io::stdout();
        if !ready {
            out.write_all(b"\x1b]0;VTCODE_STREAM_READY\x07").unwrap();
            ready = true;
        } else if app.mouse_col >= 49 && app.mouse_row >= 4 {
            let point = usize::from(app.mouse_row - 4) * 80 + usize::from(app.mouse_col - 49);
            let state = super::pointer_event_loop_latency_tests::last_pointer_updated()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let render = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            write!(
                out,
                "\x1b]0;VTCODE_STREAM_{point}_{frame}_{state}_{render}\x07"
            )
            .unwrap();
            if point == (N - 1) * 37 % 2400 && app.drag_start_pos.is_none() {
                out.write_all(b"\x1b]0;VTCODE_STREAM_DONE\x07").unwrap();
                out.flush().unwrap();
                // Let the private console render its final screen before the
                // child restores the alternate buffer and exits.
                std::thread::sleep(Duration::from_millis(100));
                return true;
            }
        }
        out.flush().unwrap();
        false
    });
    run_app(&mut terminal, &mut app, pump, None).unwrap();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )
    .unwrap();
    crossterm::terminal::disable_raw_mode().unwrap();
}
