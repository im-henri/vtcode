use super::*;
use crate::remote::{self as remote, Change, Command, RemotePath, Reply};

fn path(value: &str) -> RemotePath {
    RemotePath::new(value.as_bytes().to_vec()).unwrap()
}
fn save(secret: &str) -> Command {
    Command::Save {
        path: path("src/λ.rs"),
        base: None,
        encoding: "UTF-8".into(),
        change: Change {
            start: 4,
            end: 7,
            text: secret.into(),
        },
    }
}

#[test]
fn operation_records_have_absolute_times_ids_phases_sizes_and_save_conflicts() {
    let log = Log::default();
    let trace = log.begin("ssh".as_ref());
    let operation = trace
        .request(42, &save("PRIVATE FILE CONTENTS"), "/project")
        .unwrap();
    operation.stage("Sending request");
    operation.stage("Awaiting response");
    let error = crate::editor::save::conflict_message("Changed by another process".into());
    operation.reply(&Err(error.into()));
    // Racing late writer updates and cleanup must not append after completion.
    operation.stage("Awaiting response");
    operation.completed("too late".into());
    drop(operation);
    let snapshot = log.since(0);
    let lines: Vec<_> = snapshot
        .entries
        .iter()
        .filter(|(_, text)| text.contains("request #42"))
        .collect();
    assert_eq!(lines.len(), 4);
    for (_, line) in &lines {
        let timestamp = line.strip_prefix('[').unwrap().split(']').next().unwrap();
        chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
        assert!(timestamp.ends_with('Z') && timestamp.contains('.'));
        assert!(line.contains("[info] Operation: request #42") && line.contains(" ms"));
        assert!(line.contains("src/λ.rs") && line.contains("workspace /project"));
    }
    let report = log.report();
    assert!(report.contains("replace characters 4..7 with 21 bytes"));
    assert!(report.contains("Save conflict") && report.contains("Changed by another process"));
    assert!(report.contains("last phase: Awaiting response"));
    assert!(!report.contains("PRIVATE FILE CONTENTS") && !report.contains("too late"));
}

#[test]
fn chunk_input_and_polling_hot_paths_remain_silent_and_arguments_are_private() {
    let log = Log::default();
    let trace = log.begin("ssh".as_ref());
    let before = log.version();
    for command in [
        Command::Ping,
        Command::Heartbeat { nonce: 42 },
        Command::ProcessInput {
            process: 1,
            data: "PRIVATE KEYSTROKES".into(),
        },
        Command::Text(remote::text::Command::Append {
            transfer: 2,
            offset: 0,
            text: "PRIVATE TEXT".into(),
        }),
        Command::Text(remote::text::Command::Read {
            transfer: 2,
            start: 0,
            end: 64,
        }),
        Command::Binary(remote::binary::Command::Read {
            source: 1,
            offset: 0,
            count: 64,
        }),
        Command::Inspect {
            paths: vec![path("hidden.txt")],
        },
        Command::Files {
            generation: 1,
            known_version: Some(1),
        },
    ] {
        assert!(trace.request(1, &command, "/project").is_none());
    }
    assert_eq!(log.version(), before);
    let command = Command::Spawn {
        process: 3,
        program: "PRIVATE PROGRAM".into(),
        args: vec!["--token=PRIVATE TOKEN".into()],
        cwd: path("src"),
        terminal: None,
    };
    trace
        .request(2, &command, "/project")
        .unwrap()
        .completed(String::new());
    let report = log.report();
    assert!(report.contains("Start process") && report.contains("process #3; cwd=\"src\""));
    assert!(!report.contains("PRIVATE"));
}

#[test]
fn cancellation_timeout_and_abandonment_finish_once_with_the_last_phase() {
    let log = Log::default();
    let trace = log.begin("ssh".as_ref());
    let operation = trace
        .request(
            1,
            &Command::Open {
                path: path("missing.txt"),
            },
            "root",
        )
        .unwrap();
    operation.stage("Awaiting response");
    let copy = operation.clone();
    operation.error(&io_error(std::io::ErrorKind::TimedOut));
    copy.failed("late failure");
    drop((operation, copy));
    let operation = trace
        .request(
            2,
            &Command::Open {
                path: path("cancelled.txt"),
            },
            "root",
        )
        .unwrap();
    operation.stage("Cancellation requested");
    operation.error(&io_error(std::io::ErrorKind::Interrupted));
    drop(operation);
    drop(trace.request(
        3,
        &Command::Open {
            path: path("abandoned.txt"),
        },
        "root",
    ));
    let report = log.report();
    assert_eq!(report.matches("· Timed out ·").count(), 1);
    assert_eq!(report.matches("· Cancelled ·").count(), 1);
    assert_eq!(report.matches("· Stopped ·").count(), 1);
    assert!(!report.contains("late failure"));
}
fn io_error(kind: std::io::ErrorKind) -> std::io::Error {
    kind.into()
}

#[test]
fn filesystem_notifications_store_bounded_metadata_without_file_contents_or_paths() {
    let log = Log::default();
    let trace = log.begin("ssh".as_ref());
    let before = log.version();
    trace.workspace_changes(&remote::watch::Batch {
        root_uri: "file:///project\nforged-event".into(),
        states: vec![remote::watch::State {
            path: path("PRIVATE-file-name"), exists: true, modified: None, size: Some(123),
        }],
        structural: true, git: true, rescan: false, active: Some(true),
    });
    let snapshot = log.since(before);
    assert_eq!(snapshot.entries.len(), 1);
    let line = &snapshot.entries[0].1;
    assert!(line.contains("1 paths; structure=true; Git=true; rescan=false; watcher=active"));
    assert!(line.contains(r"file:///project\nforged-event"));
    assert!(!line.contains('\n') && !line.contains("PRIVATE"));
    assert!(trace.request(1, &Command::PerformanceSnapshot, "/project").is_none(), "capture never recursively logs its own requests");
}

#[test]
fn transfer_progress_is_sparse_and_controls_in_path_names_cannot_forge_records() {
    let log = Log::default();
    let trace = log.begin("ssh".as_ref());
    let operation = trace.transfer("Download text", 7, 10000);
    for bytes in 0..=10000 {
        operation.progress(bytes, 10000);
    }
    operation.completed("10000 bytes received".into());
    let weird = path("λ\n\t\u{1b}[31m.rs");
    trace
        .operation("Open document", &weird)
        .completed(String::new());
    let snapshot = log.since(0);
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|(_, line)| line.contains("transfer #7"))
            .count(),
        4
    );
    let report = log.report();
    assert!(report.contains("10000/10000 bytes (100%)"));
    assert!(report.contains(r"λ\n\t\u{1b}[31m.rs"));
    assert!(!report.contains('\u{1b}'));
    assert!(
        snapshot
            .entries
            .iter()
            .all(|(_, line)| !line.contains('\n'))
    );
}

#[test]
fn non_utf8_remote_names_are_distinct_from_each_other_and_literal_escape_names() {
    let log = Log::default();
    let trace = log.begin("ssh".as_ref());
    for bytes in [
        b"bad-\xff.rs".as_slice(),
        b"bad-\xfe.rs",
        br"bad-\xff.rs",
        "bad-�.rs".as_bytes(),
    ] {
        trace
            .operation("Read file", &RemotePath::new(bytes.to_vec()).unwrap())
            .completed(String::new());
    }
    let report = log.report();
    for label in [
        r#""bad-\xff.rs""#,
        r#""bad-\xfe.rs""#,
        r#""bad-\\xff.rs""#,
        "\"bad-�.rs\"",
    ] {
        assert_eq!(report.matches(label).count(), 2, "{label}: {report}");
    }
}

#[test]
fn successful_file_operations_trace_read_save_close_without_changing_wire_bytes() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.txt"), "original private text\n").unwrap();
    let mut counts = Vec::new();
    for enabled in [false, true] {
        let log = Log::default();
        let (client, counter) =
            super::tests::connect(root.path(), enabled.then(|| log.begin("ssh".as_ref())));
        let workspace = remote::workspace::Workspace::new(client.clone());
        let key = workspace.view_key(&path("file.txt"));
        let loaded = workspace.load(&key).unwrap();
        let Reply::Saved { .. } = client
            .call(Command::Save {
                path: path("file.txt"),
                base: Some(loaded.remote_checkpoint.as_ref().unwrap().revision.clone()),
                encoding: "UTF-8".into(),
                change: Change {
                    start: 0,
                    end: loaded.rope.len_chars(),
                    text: "changed private text\n".into(),
                },
            })
            .unwrap()
        else {
            panic!("save failed")
        };
        client
            .notify(Command::Close {
                path: path("file.txt"),
            })
            .unwrap();
        client.call(Command::Ping).unwrap(); // drain the preceding notification
        assert_eq!(loaded.rope.to_string(), "original private text\n");
        if enabled {
            let report = log.report();
            for label in [
                "Open document",
                "Read file",
                "Save file",
                "Close file",
                "Awaiting response",
                "Completed",
            ] {
                assert!(report.contains(label), "missing {label}: {report}");
            }
            assert!(
                !report.contains("original private text")
                    && !report.contains("changed private text")
            );
            assert!(report.contains("Close file · Sent"));
            assert!(
                !report.contains("Close file · Completed"),
                "A fire-and-forget close has no acknowledgement"
            );
        }
        counts.push(*counter.lock().unwrap());
        client.call(Command::Shutdown).unwrap();
        client.disconnect();
        std::fs::write(root.path().join("file.txt"), "original private text\n").unwrap();
    }
    assert_eq!(
        counts[0], counts[1],
        "operation logging must not change SSH traffic"
    );
}

#[test]
fn git_output_routes_remote_git_requests_without_extra_wire_bytes() {
    let root = tempfile::tempdir().unwrap();
    let _repo = git2::Repository::init(root.path()).unwrap();
    let mut counts = Vec::new();
    for enabled in [false, true] {
        let ssh = Log::default();
        let git = Log::default();
        let (client, counter) = super::tests::connect(root.path(), Some(ssh.begin("ssh".as_ref())));
        if enabled { client.set_git_output(&git); }
        client.call(Command::GitBranches { repo: path("") }).unwrap();
        let result = client.call(Command::GitNetwork {
            repo: path(""), args: vec!["pull".into()], push: false,
        }).unwrap();
        assert!(matches!(result, Reply::GitNetwork(ref output) if !output.success));
        client.call(Command::List { path: path("") }).unwrap();
        if enabled {
            let report = git.report();
            assert!(report.contains("Git branches") && report.contains("Completed"));
            assert!(report.contains("Git pull") && report.contains("stderr") && report.contains("Failed"));
            assert!(!report.contains("Read directory"));
            let report = ssh.report();
            assert!(!report.contains("Git branches") && !report.contains("Git pull"));
            assert!(report.contains("Read directory"));
        }
        counts.push(*counter.lock().unwrap());
        client.call(Command::Shutdown).unwrap();
        client.disconnect();
    }
    assert_eq!(counts[0], counts[1], "Git Output must not add SSH traffic");
}

#[test]
fn stalled_request_is_visible_before_completion_and_disconnect_identifies_pending_work() {
    use std::{io::Read, sync::mpsc};
    struct Silent(mpsc::Receiver<()>);
    impl Read for Silent {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let hello = remote::service::Service::new(root.path())
        .unwrap()
        .hello(42);
    let log = Log::default();
    let (end, reader) = mpsc::channel();
    let client = remote::client::Client::attach_with_diagnostics(
        std::io::BufReader::new(Silent(reader)),
        std::io::sink(),
        hello,
        Some(log.begin("ssh".as_ref())),
    )
    .unwrap();
    let worker_client = client.clone();
    let worker = std::thread::spawn(move || {
        worker_client.call_file(Command::Open {
            path: path("stalled.rs"),
        })
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let report = log.report();
        if report.contains("Read file · Awaiting response") {
            assert!(!report.contains("Read file · Completed"));
            assert!(report.contains("stalled.rs"));
            break;
        }
        assert!(Instant::now() < deadline, "{report}");
        std::thread::sleep(Duration::from_millis(1));
    }
    client.disconnect();
    assert!(worker.join().unwrap().is_err());
    let report = log.report();
    assert!(report.contains("Read file · Failed"), "{report}");
    assert!(report.contains("last phase: Awaiting response"));
    assert_eq!(report.matches("Read file · Failed").count(), 1);
    drop(end);
}

#[test]
fn request_timeouts_are_recorded_and_late_cleanup_does_not_claim_success() {
    use std::{io::Read, sync::mpsc};
    struct Silent(mpsc::Receiver<()>);
    impl Read for Silent {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let hello = remote::service::Service::new(root.path())
        .unwrap()
        .hello(42);
    let log = Log::default();
    let (end, reader) = mpsc::channel();
    let client = remote::client::Client::attach_with_diagnostics(
        std::io::BufReader::new(Silent(reader)),
        std::io::sink(),
        hello,
        Some(log.begin("ssh".as_ref())),
    )
    .unwrap();
    let result = client.call_timeout(
        Command::List { path: path("slow") },
        Duration::from_millis(40),
    );
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    client.disconnect();
    let report = log.report();
    assert!(report.contains("Read directory · Timed out"), "{report}");
    assert!(
        !report.contains("Read directory · Completed")
            && !report.contains("Read directory · Stopped")
    );
    drop(end);
}

#[test]
fn streamed_unicode_files_trace_whole_operations_without_per_chunk_events() {
    let root = tempfile::tempdir().unwrap();
    let original = "PRIVATE original λ🦀\n".repeat(80000);
    std::fs::write(root.path().join("large.txt"), &original).unwrap();
    let log = Log::default();
    let (client, _) = super::tests::connect(root.path(), Some(log.begin("ssh".as_ref())));
    let workspace = remote::workspace::Workspace::new(client.clone());
    let key = workspace.view_key(&path("large.txt"));
    let loaded = workspace.load(&key).unwrap();
    assert_eq!(loaded.rope.to_string(), original);
    let replacement = ropey::Rope::from_str(&"PRIVATE changed 文🦀\n".repeat(90000));
    let request = crate::editor::BackgroundSaveRequest {
        prepared_options: None,
        source_document_id: 1,
        path: key,
        rope: replacement.clone(),
        encoding: "UTF-8",
        line_ending: "LF",
        trim_trailing: false,
        insert_final_newline: false,
        source_content_version: 1,
        fail_if_exists: false,
        expected_disk: Some(crate::editor::DiskSaveBaseline {
            rope: loaded.rope.clone(),
            encoding: "UTF-8",
        }),
    };
    let saved = workspace
        .save_progress(
            &request,
            Some((loaded.remote_checkpoint.unwrap(), loaded.rope, "UTF-8")),
            None,
            |_, _| {},
        )
        .unwrap();
    assert_eq!(saved.rope, replacement);
    assert_eq!(
        std::fs::read_to_string(root.path().join("large.txt")).unwrap(),
        replacement.to_string()
    );
    let report = log.report();
    for phrase in [
        "Open document · Completed",
        "Download text",
        "Save document · Prepare save",
        "Upload file changes",
        "Commit streamed save",
        "Save document · Completed",
    ] {
        assert!(report.contains(phrase), "missing {phrase}: {report}");
    }
    assert!(
        !report.contains("PRIVATE") && !report.contains("Append") && !report.contains("Read chunk")
    );
    assert!(
        log.since(0).entries.len() < 45,
        "logging must aggregate transfer chunks"
    );
    client.call(Command::Shutdown).unwrap();
    client.disconnect();
}

#[test]
#[ignore = "Loopback read/save workload to compare diagnostics on/off; --ignored --nocapture"]
fn benchmark_file_workflows_with_and_without_diagnostics() {
    const COUNT: usize = 120;
    let root = tempfile::tempdir().unwrap();
    let text = "fn main() { println!(\"private\"); }\n".repeat(100);
    let mut results = Vec::new();
    // Alternate to avoid attributing warm filesystem/page caches to logging.
    for enabled in [false, true, true, false] {
        std::fs::write(root.path().join("file.txt"), &text).unwrap();
        let log = Log::default();
        let (client, counter) =
            super::tests::connect(root.path(), enabled.then(|| log.begin("ssh".as_ref())));
        let workspace = remote::workspace::Workspace::new(client.clone());
        let key = workspace.view_key(&path("file.txt"));
        let started = Instant::now();
        for _ in 0..COUNT {
            let loaded = workspace.load(&key).unwrap();
            client
                .call_file(Command::Save {
                    path: path("file.txt"),
                    base: Some(loaded.remote_checkpoint.unwrap().revision.clone()),
                    encoding: "UTF-8".into(),
                    change: Change {
                        start: 0,
                        end: 0,
                        text: String::new(),
                    },
                })
                .unwrap();
            client
                .call_file(Command::Close {
                    path: path("file.txt"),
                })
                .unwrap();
        }
        results.push(serde_json::json!({"enabled":enabled,
            "milliseconds_per_open_save_close":started.elapsed().as_secs_f64()*1000.0/COUNT as f64,
            "wire_bytes":*counter.lock().unwrap(), "events":log.since(0).entries.len()}));
        client.call(Command::Shutdown).unwrap();
        client.disconnect();
    }
    println!(
        "VTCODE_LOG_FILE_WORKLOAD {}",
        serde_json::json!({"iterations_per_sample":COUNT,"samples":results})
    );
}

#[test]
#[ignore = "Local diagnostics cost/throughput measurement; run with --ignored --nocapture"]
fn benchmark_metadata_tracing_and_silent_chunk_classification() {
    use std::hint::black_box;
    const COUNT: u64 = 20000;
    let log = Log::default();
    let trace = log.begin("ssh".as_ref());
    let command = save("private 100-line edit");
    let started = Instant::now();
    for id in 1..=COUNT {
        let operation = trace.request(id, black_box(&command), "/project").unwrap();
        operation.stage("Encoding request");
        operation.stage("Sending request");
        operation.stage("Awaiting response");
        operation.completed("100 bytes".into());
    }
    let elapsed = started.elapsed();
    let chunk = Command::Text(remote::text::Command::Read {
        transfer: 1,
        start: 0,
        end: 32768,
    });
    let started = Instant::now();
    for id in 1..=1_000_000 {
        assert!(black_box(trace.request(id, black_box(&chunk), "/project")).is_none());
    }
    let silent = started.elapsed();
    println!(
        "VTCODE_LOG_COST {}",
        serde_json::json!({
            "operations":COUNT, "events_per_operation":5,
            "microseconds_per_operation":elapsed.as_secs_f64()*1e6/COUNT as f64,
            "silent_chunk_nanoseconds":silent.as_secs_f64()*1e9/1_000_000.0,
            "retained_events":log.since(0).entries.len(),
        })
    );
    assert!(log.0.state.lock().unwrap().bytes <= MAX_TEXT);
}
