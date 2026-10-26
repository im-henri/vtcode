//! Real, unpaced ConPTY input and console-host memory. All setup, DLL search
//! changes, CPU affinity and injected input belong to private test processes.
use super::ConsoleInputMode;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers, PasteFailure, PasteStatus};
use std::{
    io::{self, Read, Write},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryW, SetDefaultDllDirectories},
        ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::{
            GetCurrentProcess, GetProcessAffinityMask, OpenProcess, PROCESS_QUERY_INFORMATION,
            PROCESS_TERMINATE, PROCESS_VM_READ, QueryFullProcessImageNameW, SetProcessAffinityMask,
            TerminateProcess,
        },
    },
};

const MODE: &str = "VTCODE_PASTE_MEMORY_TEST_PROCESS";
const LINE: &str = "row é🦀0123456789\n";
const READY: &str = "VTCODE_PASTE_MEMORY_READY";
const DONE: &str = "VTCODE_PASTE_MEMORY_DONE";
const MEMORY_LIMIT: usize = 512 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(300);

#[test]
fn windows_paste_memory_and_following_shortcuts() {
    run(
        "windows_input::paste_memory_tests::windows_paste_memory_and_following_shortcuts",
        200_000,
        false,
        false,
    );
}

#[test]
fn windows_paste_memory_cancel_drains_before_following_shortcuts() {
    run(
        "windows_input::paste_memory_tests::windows_paste_memory_cancel_drains_before_following_shortcuts",
        50_000,
        true,
        false,
    );
}

#[test]
#[ignore = "21 MB unpaced console paste; optional one-core contention / explicit ConPTY DLL"]
fn benchmark_windows_million_line_paste_memory() {
    run(
        "windows_input::paste_memory_tests::benchmark_windows_million_line_paste_memory",
        1_000_000,
        false,
        true,
    );
}

fn run(test: &str, lines: usize, cancel: bool, ignored: bool) {
    match std::env::var(MODE).as_deref() {
        Ok("child") => receive(lines, cancel),
        Ok("parent") => send(lines),
        _ => {
            // Never change the test runner's console or DLL search state.
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", test, "--nocapture", "--test-threads=1"]);
            if ignored {
                command.arg("--ignored");
            }
            let output = command
                .env(MODE, "parent")
                .creation_flags(0x0800_0000)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{stdout}\n{stderr}");
            print!("{stdout}");
        }
    }
}

fn receive(lines: usize, cancel: bool) {
    let mode = ConsoleInputMode::save().unwrap();
    crossterm::terminal::enable_raw_mode().unwrap();
    crossterm::execute!(
        io::stdout(),
        event::EnableMouseCapture,
        event::EnableBracketedPaste
    )
    .unwrap();
    mode.enable_virtual_terminal_input().unwrap();
    // The application uses the same scoped boost on its input decoder.
    let _priority = crate::worker_priority::InputPriority::new(true);
    println!("{READY}");
    io::stdout().flush().unwrap();
    let mut progress = 0;
    let mut cancelled = false;
    loop {
        assert!(event::poll(TIMEOUT).unwrap(), "paste stopped arriving");
        match event::read().unwrap() {
            Event::PasteStatus(PasteStatus::Receiving { bytes }) => {
                assert!(bytes > progress);
                progress = bytes;
                if cancel && !cancelled {
                    event::cancel_paste();
                }
            }
            Event::PasteStatus(PasteStatus::Failed {
                reason: PasteFailure::Cancelled,
                ..
            }) if cancel => {
                assert!(!cancelled);
                cancelled = true;
            }
            Event::Paste(text) if !cancel => {
                assert_eq!(text.len(), LINE.len() * lines);
                assert!(
                    text.as_bytes()
                        .chunks(LINE.len())
                        .all(|row| row == LINE.as_bytes())
                );
                assert!(progress > 0);
                break;
            }
            Event::Key(key) if cancel && cancelled => {
                assert_eq!(key, palette_key());
                // The cancelled payload has been drained through its delimiter.
                break;
            }
            // Do not print an entire pasted payload when a regression fails.
            Event::Paste(text) => panic!("cancelled paste applied {} bytes", text.len()),
            other => panic!("paste became {other:?}"),
        }
    }
    if !cancel {
        assert_eq!(event::read().unwrap(), Event::Key(palette_key()));
    }
    for expected in [
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL),
    ] {
        assert_eq!(event::read().unwrap(), Event::Key(expected));
    }
    // Consume the end of the native batch and return to idle before exit.
    assert!(!event::poll(Duration::ZERO).unwrap());
    println!("{DONE}");
    io::stdout().flush().unwrap();
}

fn palette_key() -> KeyEvent {
    KeyEvent::new(
        KeyCode::Char('P'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    )
}

struct Process(OwnedHandle);
impl Process {
    fn open(pid: u32) -> Self {
        // These PIDs identify only this fixture's child and newly created host.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        assert!(
            !handle.is_null(),
            "test process {pid}: {}",
            io::Error::last_os_error()
        );
        Self(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle()
    }
    fn peak_memory(&self) -> usize {
        let mut counters = PROCESS_MEMORY_COUNTERS::default();
        let size = std::mem::size_of_val(&counters) as u32;
        counters.cb = size;
        if unsafe { GetProcessMemoryInfo(self.raw(), &mut counters, size) } == 0 {
            return 0;
        }
        // Check committed memory too: paging/trimming must not hide a huge
        // console queue merely because its current resident set is smaller.
        counters.PeakWorkingSetSize.max(counters.PeakPagefileUsage)
    }
    fn image(&self) -> String {
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        assert_ne!(
            unsafe { QueryFullProcessImageNameW(self.raw(), 0, path.as_mut_ptr(), &mut length) },
            0
        );
        String::from_utf16_lossy(&path[..length as usize])
    }
}

struct Stop {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    host: Option<Process>,
    running: Arc<AtomicBool>,
}
impl Drop for Stop {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.child.kill();
        if let Some(host) = &self.host {
            // The handle cannot be reused for an unrelated PID after exit.
            unsafe {
                TerminateProcess(host.raw(), 1);
            }
        }
    }
}

fn hosts() -> Vec<(u32, u32)> {
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    assert_ne!(raw, INVALID_HANDLE_VALUE);
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = PROCESSENTRY32W::default();
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    let mut ok = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) };
    let mut result = Vec::new();
    while ok != 0 {
        let length = entry
            .szExeFile
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
        if name.eq_ignore_ascii_case("conhost.exe") || name.eq_ignore_ascii_case("OpenConsole.exe")
        {
            result.push((entry.th32ProcessID, entry.th32ParentProcessID));
        }
        ok = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) };
    }
    result
}

fn send(lines: usize) {
    let loaded = std::env::var_os("VTCODE_PASTE_CPU_CONTENTION").is_some();
    if loaded {
        let mut allowed = 0;
        let mut system = 0;
        let process = unsafe { GetCurrentProcess() };
        assert_ne!(
            unsafe { GetProcessAffinityMask(process, &mut allowed, &mut system) },
            0
        );
        assert_ne!(allowed, 0);
        assert_ne!(
            unsafe { SetProcessAffinityMask(process, 1usize << allowed.trailing_zeros()) },
            0
        );
    }
    if let Some(dll) = std::env::var_os("VTCODE_PASTE_CONPTY_DLL") {
        assert!(
            std::path::Path::new(&dll).is_absolute(),
            "use an absolute test-runtime path"
        );
        let name: Vec<u16> = dll.encode_wide().chain(Some(0)).collect();
        assert!(!unsafe { LoadLibraryW(name.as_ptr()) }.is_null());
    } else {
        // portable-pty otherwise searches PATH for conpty.dll, possibly loading
        // an unrelated terminal's old runtime. Pin this fixture to the system.
        assert_ne!(
            unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) },
            0
        );
    }
    let before = hosts();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
    // Replay the same exact fixture, with its original ignored/line-count mode.
    command.args(std::env::args_os().skip(1));
    command.env(MODE, "child");
    let child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let running = Arc::new(AtomicBool::new(true));
    let mut child = Stop {
        child,
        host: None,
        running: running.clone(),
    };
    let reader_process = Process::open(child.child.process_id().unwrap());
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let (tx, rx) = mpsc::channel();
    let reading = std::thread::spawn(move || {
        let mut buffer = [0; 4096];
        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 || tx.send(buffer[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut output = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while !contains(&output, READY) {
        output.extend(
            rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("console startup stalled"),
        );
    }
    let created: Vec<_> = hosts()
        .into_iter()
        .filter(|&(pid, parent)| {
            parent == std::process::id() && !before.iter().any(|&(old, _)| old == pid)
        })
        .collect();
    assert_eq!(
        created.len(),
        1,
        "could not identify the private host: {created:?}"
    );
    child.host = Some(Process::open(created[0].0));
    let image = child.host.as_ref().unwrap().image();
    let mut competitors = Vec::new();
    if loaded {
        for _ in 0..2 {
            let running = running.clone();
            competitors.push(std::thread::spawn(move || {
                let mut value = 1u64;
                while running.load(Ordering::Relaxed) {
                    for _ in 0..1024 {
                        value = std::hint::black_box(value.wrapping_mul(17).wrapping_add(5));
                    }
                }
            }));
        }
    }
    let started = Instant::now();
    let writing = std::thread::spawn(move || {
        let mut text = String::with_capacity(LINE.len() * lines + 100);
        text.push_str("\x1b[200~");
        for _ in 0..lines {
            text.push_str(LINE);
        }
        text.push_str("\x1b[201~\x1b[80;25;16;1;24;1_\x1b[13;28;13;1;16;1_\x1b[8;14;127;1;8;1_");
        // Deliberately unpaced. A bounded producer would conceal queue growth.
        let result = writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.flush());
        (result, writer)
    });
    let mut host_peak = 0;
    let mut reader_peak = 0;
    let status = loop {
        host_peak = host_peak.max(child.host.as_ref().unwrap().peak_memory());
        reader_peak = reader_peak.max(reader_process.peak_memory());
        assert!(
            host_peak < MEMORY_LIMIT && reader_peak < MEMORY_LIMIT,
            "console memory runaway: host {host_peak} bytes; reader {reader_peak} bytes; {image}"
        );
        assert!(
            started.elapsed() < TIMEOUT,
            "paste stalled: {}",
            String::from_utf8_lossy(&output)
        );
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(10)) {
            output.extend(bytes);
        }
        assert!(
            output.len() < 1024 * 1024,
            "unexpected console output flood"
        );
        if let Some(status) = child.child.try_wait().unwrap() {
            break status;
        }
    };
    running.store(false, Ordering::Relaxed);
    for competitor in competitors {
        competitor.join().unwrap();
    }
    while let Ok(bytes) = rx.recv_timeout(Duration::from_millis(25)) {
        output.extend(bytes);
    }
    let (written, writer) = writing.join().unwrap();
    assert!(written.is_ok(), "paste transport: {written:?}");
    drop(writer);
    drop(pair.master);
    reading.join().unwrap();
    assert!(
        status.success() && contains(&output, DONE),
        "{}",
        String::from_utf8_lossy(&output)
    );
    println!(
        "{} bytes; elapsed {:?}; peak host {:.1} MiB; peak reader {:.1} MiB; one-core contention={loaded}; host={image}",
        LINE.len() * lines,
        started.elapsed(),
        host_peak as f64 / 1048576.0,
        reader_peak as f64 / 1048576.0
    );
}

fn contains(output: &[u8], marker: &str) -> bool {
    output
        .windows(marker.len())
        .any(|part| part == marker.as_bytes())
}
