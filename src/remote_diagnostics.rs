//! Local, event-driven connection and operation diagnostics. Producers append
//! bounded records; timestamps and operation descriptions are rendered only by
//! the viewer. No headless work, protocol traffic, disk writes or idle polling.
#[path = "remote_diagnostic_operations.rs"]
mod operations;
pub(crate) use operations::Operation;
#[cfg(test)]
#[path = "remote_diagnostic_operation_tests.rs"]
mod operation_tests;
use crate::background::UiWake;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

const MAX_ENTRIES: usize = 32 * 1024;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_ENTRY: usize = 2048;

#[derive(Clone)]
pub(crate) struct Log(Arc<Inner>);
struct Inner {
    start: Instant,
    wall: SystemTime,
    version: AtomicU64,
    next_operation: AtomicU64,
    state: Mutex<History>,
    observer: Mutex<Option<UiWake>>,
}
#[derive(Default)]
struct History {
    attempt: u64,
    entries: VecDeque<Arc<Entry>>,
    bytes: usize,
    omitted: u64,
}
struct Entry {
    elapsed: Duration,
    attempt: u64,
    source: &'static str,
    body: Body,
}
enum Body {
    Message(String),
    WorkspaceChanges {
        workspace: String,
        paths: usize,
        structural: bool,
        git: bool,
        rescan: bool,
        active: Option<bool>,
    },
    Operation {
        description: Arc<operations::Description>,
        phase: &'static str,
        duration: Duration,
        detail: String,
    },
}
impl Body {
    fn bytes(&self) -> usize {
        match self {
            Self::Message(text) => text.len(),
            Self::WorkspaceChanges { workspace, .. } => workspace.len() + 128,
            Self::Operation {
                description,
                detail,
                ..
            } => description.target.len() + detail.len() + 128,
        }
    }
}
impl Entry {
    fn level(&self) -> &'static str {
        match &self.body {
            Body::Operation { phase: "Failed", .. } => "error",
            Body::Operation { phase: "Stopped" | "Cancelled", .. } => "warning",
            _ => match self.source {
                "Error" | "Launch failed" | "Setup failed" | "Reconnect failed" => "error",
                "Disconnected" | "Retry" | "Transport exit" | "Local helper exit" => "warning",
                _ => "info",
            },
        }
    }

    fn line(&self, origin: SystemTime, include_relative_time: bool) -> String {
        use std::fmt::Write;
        let wall: chrono::DateTime<chrono::Utc> =
            origin.checked_add(self.elapsed).unwrap_or(origin).into();
        let mut line = String::with_capacity(self.body.bytes() + 100);
        let _ = write!(
            line,
            "[{}] ",
            wall.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        // The tab keeps an absolute timestamp without repeating connection-
        // elapsed time on every row. Export includes both for investigations.
        if include_relative_time {
            let _ = write!(line, "[+{:.3}s] ", self.elapsed.as_secs_f64());
        }
        let _ = write!(line, "[{}] ", self.level());
        let _ = write!(line, "{}: ", self.source);
        match &self.body {
            Body::Message(text) => line.push_str(text),
            Body::WorkspaceChanges { workspace, paths, structural, git, rescan, active } => {
                let watcher = active.map_or("unchanged", |active| if active { "active" } else { "unavailable" });
                let _ = write!(line, "{paths} paths; structure={structural}; Git={git}; rescan={rescan}; watcher={watcher}; workspace={workspace}");
            }
            Body::Operation {
                description,
                phase,
                duration,
                detail,
            } => {
                let _ = write!(
                    line,
                    "{} #{} {} · {} · {:.3} ms",
                    description.scope,
                    description.id,
                    description.name,
                    phase,
                    duration.as_secs_f64() * 1000.0
                );
                if !description.target.is_empty() {
                    let _ = write!(line, " · {}", description.target);
                }
                if !detail.is_empty() {
                    let _ = write!(line, " · {detail}");
                }
            }
        }
        line
    }
}
pub(crate) struct Snapshot {
    pub version: u64,
    pub first: u64,
    pub omitted: u64,
    pub entries: Vec<(u64, String)>,
}
#[derive(Clone)]
pub(crate) struct Attempt {
    pub log: Log,
    pub id: u64,
}
impl Default for Log {
    fn default() -> Self {
        Self(Arc::new(Inner {
            start: Instant::now(),
            wall: SystemTime::now(),
            version: AtomicU64::new(0),
            next_operation: AtomicU64::new(1),
            state: Mutex::default(),
            observer: Mutex::default(),
        }))
    }
}
impl Log {
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn version(&self) -> u64 {
        self.0.version.load(Ordering::Acquire)
    }
    pub(crate) fn observe(&self, wake: Option<UiWake>) {
        *self.0.observer.lock().unwrap() = wake;
    }
    pub(crate) fn begin(&self, program: &std::ffi::OsStr) -> Attempt {
        let id = {
            let mut state = self.0.state.lock().unwrap();
            state.attempt = state.attempt.saturating_add(1);
            state.attempt
        };
        let attempt = Attempt {
            log: self.clone(),
            id,
        };
        // Arguments can contain credentials. Do not record them or input.
        attempt.event(
            "Connection attempt",
            &format!(
                "{id} · Starting {}",
                std::path::Path::new(program)
                    .file_name()
                    .unwrap_or(program)
                    .to_string_lossy()
            ),
        );
        attempt
    }
    pub(crate) fn event(&self, source: &'static str, text: &str) {
        let id = self.0.state.lock().unwrap().attempt;
        self.push(id, source, text);
    }
    fn push(&self, attempt: u64, source: &'static str, text: &str) {
        let text = plain(text.as_bytes());
        if text.trim().is_empty() {
            return;
        }
        self.append(attempt, source, Body::Message(text));
    }
    fn append(&self, attempt: u64, source: &'static str, body: Body) {
        let mut state = self.0.state.lock().unwrap();
        state.bytes += body.bytes();
        state.entries.push_back(Arc::new(Entry {
            elapsed: self.0.start.elapsed(),
            attempt,
            source,
            body,
        }));
        while state.bytes > MAX_TEXT || state.entries.len() > MAX_ENTRIES {
            let old = state.entries.pop_front().unwrap();
            state.bytes -= old.body.bytes();
            state.omitted += 1;
        }
        self.0.version.fetch_add(1, Ordering::Release);
        drop(state);
        // Only an open diagnostics view subscribes. No timer or idle wakeups.
        if let Some(wake) = &*self.0.observer.lock().unwrap() {
            wake.notify();
        }
    }
    pub(crate) fn report(&self) -> String {
        use std::fmt::Write;
        let wall: chrono::DateTime<chrono::Utc> = self.0.wall.into();
        // Copy references under a short lock. Formatting/copying a full retained
        // MiB must never block the transport workers which append to this log.
        let (omitted, entries) = {
            let state = self.0.state.lock().unwrap();
            (
                state.omitted,
                state.entries.iter().cloned().collect::<Vec<_>>(),
            )
        };
        let mut report = format!(
            "VTCode {} · protocol {} · {}\nConnection diagnostics started {}\nUTC timestamps; durations use a monotonic clock. Request IDs are per connection attempt.\nOperation metadata and transport errors only; file contents, input and command arguments are excluded.\nAwaiting response includes remote execution and network delay; it does not identify a server-side stall.\nAn EOF alone cannot identify what killed the remote process.\n\n",
            env!("CARGO_PKG_VERSION"),
            super::VERSION,
            std::env::consts::OS,
            wall.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        if omitted > 0 {
            let _ = writeln!(report, "[{omitted} older entries omitted]\n");
        }
        for entry in &entries {
            let _ = writeln!(report, "{}", entry.line(self.0.wall, true));
        }
        report
    }
    /// Copy only new events for a visible viewer. An unchanged log needs only
    /// the atomic version check, and appending never re-wraps the retained MiB.
    #[cfg(test)]
    pub(crate) fn since(&self, after: u64) -> Snapshot {
        self.batch(after, usize::MAX, usize::MAX)
    }
    pub(crate) fn batch(&self, after: u64, limit: usize, byte_budget: usize) -> Snapshot {
        let (version, omitted, skip, entries) = {
            let state = self.0.state.lock().unwrap();
            let skip = after
                .saturating_sub(state.omitted)
                .min(state.entries.len() as u64) as usize;
            let mut bytes = 0usize;
            let entries = state
                .entries
                .iter()
                .skip(skip)
                .take(limit.max(1))
                .take_while(|entry| {
                    if bytes > 0 && bytes.saturating_add(entry.body.bytes()) > byte_budget {
                        return false;
                    }
                    bytes = bytes.saturating_add(entry.body.bytes());
                    true
                })
                .cloned()
                .collect::<Vec<_>>();
            // A partial batch's cursor is the last delivered ID, not the
            // collector's newest version. Remaining work wakes the next frame.
            (
                state.omitted + (skip + entries.len()) as u64,
                state.omitted,
                skip,
                entries,
            )
        };
        let first = omitted + 1;
        let entries = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                (
                    first + (skip + index) as u64,
                    entry.line(self.0.wall, false),
                )
            })
            .collect();
        Snapshot {
            version,
            first,
            omitted,
            entries,
        }
    }
    pub(crate) fn failure_context(&self, attempt: u64) -> String {
        let state = self.0.state.lock().unwrap();
        let lines: Vec<_> = state
            .entries
            .iter()
            .rev()
            .filter(|entry| {
                entry.attempt == attempt
                    && matches!(
                        entry.source,
                        "Transport output" | "Console" | "Transport exit" | "Local helper exit"
                    )
            })
            .take(3)
            .collect();
        lines
            .into_iter()
            .rev()
            .filter_map(|entry| match &entry.body {
                Body::Message(text) => Some(format!("{}: {}", entry.source, text)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
impl Attempt {
    pub(crate) fn event(&self, source: &'static str, text: &str) {
        self.log.push(self.id, source, text);
    }
    pub(crate) fn output(&self, source: &'static str, bytes: &[u8]) {
        // This guard is also used before the handshake. Framed editor/file
        // contents and PTY echo must never become diagnostic text.
        if super::wire::record(bytes).starts_with(b"VTCR") {
            return;
        }
        let text = String::from_utf8_lossy(bytes);
        self.log.push(self.id, source, &text);
    }
    pub(crate) fn error(&self, error: std::io::Error) -> std::io::Error {
        let context = self.log.failure_context(self.id);
        if context.is_empty() {
            error
        } else {
            std::io::Error::new(error.kind(), format!("{error}\n{context}"))
        }
    }
}

fn plain(bytes: &[u8]) -> String {
    struct Text {
        value: String,
        truncated: bool,
    }
    impl vte::Perform for Text {
        fn print(&mut self, c: char) {
            if self.value.len() + c.len_utf8() <= MAX_ENTRY {
                self.value.push(c);
            } else {
                self.truncated = true;
            }
        }
        fn execute(&mut self, byte: u8) {
            if matches!(byte, b'\n' | b'\r' | b'\t') {
                self.print(if byte == b'\t' { ' ' } else { '\n' });
            }
        }
    }
    let mut text = Text {
        value: String::new(),
        truncated: false,
    };
    let mut parser = vte::Parser::new();
    for &byte in bytes.iter().take(super::wire::MAX_LINE) {
        parser.advance(&mut text, byte);
    }
    if bytes.len() > super::wire::MAX_LINE || text.truncated {
        text.value.push_str(" [truncated]");
    }
    text.value.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_attempts_have_their_own_rows_without_per_message_fields() {
        let log = Log::default();
        let first = log.begin("ssh".as_ref());
        first.event("Handshake", "Connected");
        first.event("Disconnected", "Connection lost");
        let second = log.begin("ssh".as_ref());
        second.event("Handshake", "Reconnected");

        // Read one record at a time, as Output may deliver attempt markers and
        // subsequent messages in separate batches.
        let mut cursor = 0;
        let mut lines = Vec::new();
        while cursor < log.version() {
            let batch = log.batch(cursor, 1, usize::MAX);
            cursor = batch.version;
            lines.extend(batch.entries.into_iter().map(|(_, line)| line));
        }
        assert_eq!(lines.len(), 5);
        assert!(lines[0].ends_with("[info] Connection attempt: 1 · Starting ssh"));
        assert!(lines[1].ends_with("[info] Handshake: Connected"));
        assert!(lines[2].ends_with("[warning] Disconnected: Connection lost"));
        assert!(lines[3].ends_with("[info] Connection attempt: 2 · Starting ssh"));
        assert!(lines[4].ends_with("[info] Handshake: Reconnected"));
        let report = log.report();
        for text in lines.iter().map(String::as_str).chain([report.as_str()]) {
            assert!(!text.contains("[a1]") && !text.contains("[a2]"));
            assert!(!text.contains("[attempt "));
        }
        for attempt in [1, 2] {
            assert_eq!(
                report
                    .matches(&format!("Connection attempt: {attempt} · Starting ssh"))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn history_is_bounded_retains_latest_reason_and_escapes_terminal_controls() {
        let log = Log::default();
        let attempt = log.begin("ssh".as_ref());
        for _ in 0..(MAX_TEXT / MAX_ENTRY + 10) {
            attempt.output("Transport output", "λ".repeat(5000).as_bytes());
        }
        attempt.output(
            "Transport output",
            b"\x1b[31mPermission denied (publickey).\x1b[0m\x1b]52;c;c2VjcmV0\x07\n",
        );
        let report = log.report();
        assert!(report.contains("Permission denied (publickey)."));
        assert!(report.contains("older entries omitted"));
        assert!(!report.contains('\x1b') && !report.contains("c2VjcmV0"));
        let state = log.0.state.lock().unwrap();
        assert!(state.entries.len() <= MAX_ENTRIES && state.bytes <= MAX_TEXT);
    }
    #[test]
    fn history_retains_a_mebibyte_instead_of_dropping_after_192_events_or_64_kib() {
        let log = Log::default();
        for index in 0..1024 {
            log.event("Test", &format!("{index:04}:{}", "x".repeat(1019)));
        }
        let snapshot = log.since(0);
        assert_eq!(snapshot.entries.len(), 1024);
        assert_eq!(snapshot.omitted, 0);
        assert!(log.report().len() >= 1024 * 1024);
        log.event("Test", "latest reason");
        let snapshot = log.since(snapshot.version);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.omitted, 1);
        assert_eq!(snapshot.first, 2);
        assert!(snapshot.entries[0].1.contains("latest reason"));
        assert!(log.since(snapshot.version).entries.is_empty());
    }
    #[test]
    fn protocol_payloads_and_other_attempts_never_become_failure_context() {
        let log = Log::default();
        let first = log.begin("ssh".as_ref());
        first.output("Transport output", b"Connection refused\n");
        let second = log.begin("ssh".as_ref());
        let before = log.version();
        second.output(
            "Transport output",
            b"\x1b]777;vtcode-headless;VTCR16S:secret-file-text\x07\n",
        );
        assert_eq!(log.version(), before);
        assert!(log.failure_context(second.id).is_empty());
        second.output("Transport output", b"Host key verification failed.\n");
        let error = second.error(std::io::Error::other("SSH ended")).to_string();
        assert!(error.contains("Host key verification failed."));
        assert!(
            !error.contains("Connection refused") && !log.report().contains("secret-file-text")
        );
    }
    #[test]
    fn diagnostics_have_no_idle_wakes_and_only_wake_an_open_view() {
        let log = Log::default();
        let wake = UiWake::default();
        let initial = wake.generation();
        log.event("Test", "hidden");
        assert_eq!(wake.generation(), initial);
        log.observe(Some(wake.clone()));
        log.event("Test", "visible");
        assert_ne!(wake.generation(), initial);
        log.observe(None);
        let last = wake.generation();
        log.event("Test", "hidden again");
        assert_eq!(wake.generation(), last);
    }

    pub(super) fn connect(
        root: &std::path::Path,
        trace: Option<Attempt>,
    ) -> (super::super::client::Client, Arc<Mutex<usize>>) {
        use std::{
            io::{BufReader, Write},
            net::{TcpListener, TcpStream},
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        stream.set_nodelay(true).unwrap();
        let (server, _) = listener.accept().unwrap();
        server.set_nodelay(true).unwrap();
        let root = root.to_owned();
        std::thread::spawn(move || {
            super::super::service::serve(&root, server.try_clone().unwrap(), server).unwrap()
        });
        struct Count {
            stream: TcpStream,
            count: Arc<Mutex<usize>>,
        }
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                // A reply can reach the caller before this writer gets CPU
                // again. Snapshot under the same guard to include that write.
                let mut count = self.count.lock().unwrap();
                let n = self.stream.write(bytes)?;
                *count += n;
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.stream.flush()
            }
        }
        let count = Arc::new(Mutex::new(0));
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let hello = super::super::client::Client::read_hello(&mut reader).unwrap();
        let client = super::super::client::Client::attach_with_diagnostics(
            reader,
            Count {
                stream,
                count: count.clone(),
            },
            hello,
            trace,
        )
        .unwrap();
        (client, count)
    }

    #[test]
    fn normal_rpc_traffic_adds_no_diagnostic_events_or_wire_bytes() {
        let root = tempfile::tempdir().unwrap();
        let log = Log::default();
        let trace = log.begin("ssh".as_ref());
        let mut bytes = Vec::new();
        for diagnostics in [None, Some(trace)] {
            let (client, counter) = connect(root.path(), diagnostics);
            let version = log.version();
            for _ in 0..64 {
                client.call(super::super::Command::Ping).unwrap();
            }
            assert_eq!(
                log.version(),
                version,
                "RPC responses must never touch the diagnostic log"
            );
            bytes.push(*counter.lock().unwrap());
            client.call(super::super::Command::Shutdown).unwrap();
            client.disconnect();
        }
        assert_eq!(
            bytes[0], bytes[1],
            "Diagnostics must add no requests or bytes to SSH"
        );
    }

    #[test]
    fn attached_client_keeps_transport_errors_even_without_trailing_newline() {
        use super::super::{Reply, Response, client::Client, service::Service, wire};
        let root = tempfile::tempdir().unwrap();
        let hello = Service::new(root.path()).unwrap().hello(42);
        let log = Log::default();
        let trace = log.begin("ssh".as_ref());
        let mut bytes = b"ssh: connection reset by peer\n".to_vec();
        wire::write_message(
            &mut bytes,
            'S',
            42,
            7,
            &wire::encode(&Response {
                id: 7,
                result: Ok(Reply::Formatted("private-editor-buffer-λ".into())),
            })
            .unwrap(),
        )
        .unwrap();
        bytes.extend_from_slice(b"fatal: scheduler allocation ended");
        let client = Client::attach_with_diagnostics(
            std::io::BufReader::new(std::io::Cursor::new(bytes)),
            std::io::sink(),
            hello,
            Some(trace),
        )
        .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while client.disconnect_reason().is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        client.disconnect();
        let report = loop {
            let report = log.report();
            if report.contains("Disconnected:") {
                break report;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert!(
            report.contains("connection reset by peer")
                && report.contains("scheduler allocation ended")
        );
        assert!(!report.contains("private-editor-buffer"));
        assert_eq!(report.matches("Disconnected:").count(), 1);
        assert!(
            client
                .disconnect_reason()
                .unwrap()
                .contains("Truncated remote frame")
        );
    }
}
