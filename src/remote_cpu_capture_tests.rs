use super::*;
use crate::remote::observability::Thread;
use std::{
    net::{TcpListener, TcpStream},
    time::Instant,
};

fn sample(at: u64, cpu: Option<u64>) -> Snapshot {
    Snapshot {
        pid: 42,
        clock_ns: at,
        cpu_ns: cpu,
        threads_complete: true,
        thread_resolution_ns: Some(10_000_000),
        ..Snapshot::default()
    }
}
fn thread(id: u64, started: u64, cpu_ns: u64, name: &str) -> Thread {
    Thread {
        id,
        started,
        cpu_ns,
        name: name.into(),
        state: 'S',
    }
}
#[test]
fn cpu_uses_remote_elapsed_time_one_core_units_and_sorts_busy_threads_first() {
    let mut before = sample(1_000_000_000, Some(2_000_000_000));
    before.threads = vec![thread(1, 10, 0, "idle"), thread(2, 10, 0, "headless-git")];
    let mut after = sample(11_000_000_000, Some(2_125_000_000));
    after.threads = vec![
        thread(1, 10, 0, "idle"),
        thread(2, 10, 120_000_000, "headless-git"),
    ];
    let rows = report(&before, &after);
    assert!(rows[0].contains("1.250% (125.000 ms)"), "{rows:?}");
    assert!(rows[0].contains("over 10.000s"));
    assert!(rows[0].contains("100% = one logical CPU"));
    let text = rows.join("\n");
    assert!(text.find("headless-git").unwrap() < text.find("[idle]").unwrap());
    assert!(text.contains("10.000 ms resolution"));
    assert!(text.contains("below OS counter resolution or idle"));
    after.cpu_ns = Some(22_000_000_000);
    assert!(report(&before, &after)[0].contains("200.000%"));
}
#[test]
fn reused_new_exited_and_reset_threads_never_receive_a_fabricated_cpu_delta() {
    let mut before = sample(1, Some(100));
    before.threads = vec![
        thread(1, 10, 500, "reused-old"),
        thread(2, 10, 80, "ended"),
        thread(3, 10, 80, "reset"),
    ];
    let mut after = sample(1_000_000_001, Some(101));
    after.threads = vec![
        thread(1, 20, 550, "reused-new"),
        thread(3, 10, 20, "reset"),
        thread(4, 30, 10, "new"),
    ];
    let text = report(&before, &after).join("\n");
    assert!(text.contains("0 matched, 3 new/reset, 2 ended/unreadable"));
    assert!(!text.contains("Thread 1 [") && !text.contains("Thread 3 ["));
    assert!(text.contains("process total still includes exited threads"));
}
#[test]
fn unavailable_reset_or_invalid_process_samples_do_not_claim_idle_cpu() {
    let before = sample(100, Some(100));
    for cpu in [None, Some(99)] {
        let text = report(&before, &sample(200, cpu)).join("\n");
        assert!(text.contains("CPU unavailable"));
        assert!(!text.contains("CPU 0.000%"));
    }
    for clock in [0, 100] {
        assert!(report(&before, &sample(clock, Some(100)))[0].contains("clock did not advance"));
    }
    let mut other = sample(200, Some(100));
    other.pid += 1;
    assert!(report(&before, &other)[0].contains("process changed"));
    let mut unsupported = sample(200, None);
    unsupported.threads_complete = false;
    unsupported.thread_resolution_ns = None;
    assert!(
        report(&before, &unsupported)
            .join("\n")
            .contains("partial/unavailable")
    );
}

struct Connection {
    client: Client,
    socket: TcpStream,
    written: Arc<Mutex<Vec<u8>>>,
    log: diagnostics::Log,
    worker: Option<std::thread::JoinHandle<io::Result<()>>>,
    _root: tempfile::TempDir,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.client.disconnect();
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn connect() -> Connection {
    struct Recorded(TcpStream, Arc<Mutex<Vec<u8>>>);
    impl Write for Recorded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut output = self.1.lock().unwrap();
            let count = self.0.write(bytes)?;
            output.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    socket.set_nodelay(true).unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nodelay(true).unwrap();
    let service_root = root.path().to_owned();
    let worker = std::thread::spawn(move || {
        service::serve(&service_root, server.try_clone().unwrap(), server)
    });
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let hello = Client::read_hello(&mut reader).unwrap();
    let log = diagnostics::Log::default();
    let written: Arc<Mutex<Vec<u8>>> = Arc::default();
    let client = Client::attach_with_diagnostics(
        reader,
        Recorded(socket.try_clone().unwrap(), written.clone()),
        hello,
        Some(log.begin("ssh".as_ref())),
    )
    .unwrap();
    client.call(Command::Ping).unwrap(); // includes the output worker's initial newline
    Connection {
        client,
        socket,
        written,
        log,
        worker: Some(worker),
        _root: root,
    }
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "capture did not progress"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn commands(f: &Connection) -> Vec<Command> {
    let bytes = f.written.lock().unwrap().clone();
    let mut decoder = wire::Decoder::new('C', f.client.hello().session);
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .filter_map(|line| {
            decoder
                .line(line)
                .unwrap()
                .map(|(_, bytes)| serde_json::from_slice::<Request>(&bytes).unwrap().command)
        })
        .collect()
}
#[test]
fn capture_is_explicit_sends_exactly_two_requests_and_stops_completely() {
    let f = connect();
    let initial = f.written.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(
        f.written.lock().unwrap().len(),
        initial,
        "logging starts no sampling"
    );
    assert!(!f.client.capturing_cpu());
    assert!(f.client.capture_cpu_for(Duration::from_millis(20)).unwrap());
    wait_for(|| !f.client.capturing_cpu());
    let requests = commands(&f);
    assert_eq!(
        requests
            .iter()
            .filter(|command| matches!(command, Command::PerformanceSnapshot))
            .count(),
        2
    );
    assert_eq!(requests.len(), 3); // initial ping + two snapshots
    let end = f.written.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        f.written.lock().unwrap().len(),
        end,
        "no recurring monitor survives a capture"
    );
    let report = f.log.report();
    assert!(report.contains("Capture complete") && report.contains("100% = one logical CPU"));
    assert!(
        !report.contains("Operation:"),
        "sampling does not recursively log itself"
    );
}
#[test]
fn duplicate_capture_is_rejected_and_disconnect_interrupts_the_sleep() {
    let f = connect();
    assert!(f.client.capture_cpu_for(Duration::from_secs(10)).unwrap());
    assert!(!f.client.capture_cpu().unwrap());
    wait_for(|| f.log.report().contains("Baseline received"));
    f.client.disconnect();
    wait_for(|| !f.client.capturing_cpu());
    assert!(f.log.report().contains("Capture stopped"));
    assert_eq!(
        commands(&f)
            .iter()
            .filter(|command| matches!(command, Command::PerformanceSnapshot))
            .count(),
        1
    );
    assert!(f.client.capture_cpu().is_err());
}
#[test]
fn transport_death_during_capture_is_reported_and_cannot_leave_the_button_busy() {
    let f = connect();
    assert!(f.client.capture_cpu_for(Duration::from_secs(10)).unwrap());
    wait_for(|| f.log.report().contains("Baseline received"));
    f.socket.shutdown(std::net::Shutdown::Both).unwrap();
    wait_for(|| !f.client.capturing_cpu());
    assert!(!f.client.connected());
    assert!(f.log.report().contains("Capture stopped"));
}
