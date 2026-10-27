use super::*;
use std::time::Duration;

#[test]
fn terminal_input_is_bounded_by_bytes_and_preserves_bursty_keys_and_pastes() {
    let queue = Arc::new((Mutex::new(InputQueue::default()), Condvar::new()));
    let bytes: Vec<_> = (0..WINDOW).map(|i| (i % 251) as u8).collect();
    {
        let mut input = queue.0.lock().unwrap();
        for byte in &bytes {
            input.push(vec![*byte], WINDOW).unwrap();
        }
        assert_eq!(
            input.push(vec![0], WINDOW).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(input.bytes, WINDOW);
    }
    let mut actual = Vec::new();
    while actual.len() < bytes.len() {
        actual.extend(InputQueue::receive(&queue).unwrap());
    }
    assert_eq!(actual, bytes);
    let paste = "\x1b[200~猫\n  indented\x1b[201~".repeat(4096).into_bytes();
    queue
        .0
        .lock()
        .unwrap()
        .push(paste.clone(), 4 * 1024 * 1024)
        .unwrap();
    assert_eq!(InputQueue::receive(&queue).unwrap(), paste);
    let mut input = queue.0.lock().unwrap();
    assert_eq!(input.bytes, 0);
    input.closed = true;
    assert!(input.push(vec![1], WINDOW).is_err());
    drop(input);
    assert!(InputQueue::receive(&queue).is_none());
}

#[test]
fn output_window_stops_an_unread_process_and_credits_resume_it_losslessly() {
    for (window, block) in [(WINDOW, BLOCK), (OUTPUT_WINDOW, OUTPUT_BLOCK)] {
        let flow = Arc::new((
            Mutex::new(Flow {
                credit: window,
                window,
                closed: false,
            }),
            Condvar::new(),
        ));
        let data: Vec<_> = (0..window + block).map(|i| (i % 251) as u8).collect();
        let input = data.clone();
        let worker_flow = flow.clone();
        let (output, received) = mpsc::sync_channel(64);
        let worker = std::thread::spawn(move || {
            stream_output(1, io::Cursor::new(input), &worker_flow, &output, block)
        });
        let mut actual = Vec::new();
        let receive = || {
            let response = received.recv_timeout(Duration::from_secs(5)).unwrap();
            let Reply::ProcessData { process: 1, data } = response.result.unwrap() else {
                panic!()
            };
            URL_SAFE_NO_PAD.decode(data).unwrap()
        };
        while actual.len() < window {
            actual.extend(receive());
        }
        assert_eq!(flow.0.lock().unwrap().credit, 0);
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        flow.0.lock().unwrap().credit += block;
        flow.1.notify_one();
        actual.extend(receive());
        worker.join().unwrap().unwrap();
        assert_eq!(actual, data);
    }
}

#[test]
fn output_delivers_small_echo_without_waiting_to_fill_a_bulk_block() {
    struct Echo {
        release: mpsc::Receiver<()>,
        sent: bool,
    }
    impl Read for Echo {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.sent {
                self.release.recv().unwrap();
                return Ok(0);
            }
            self.sent = true;
            bytes[..4].copy_from_slice(b"echo");
            Ok(4)
        }
    }
    let flow = Arc::new((
        Mutex::new(Flow {
            credit: OUTPUT_WINDOW,
            window: OUTPUT_WINDOW,
            closed: false,
        }),
        Condvar::new(),
    ));
    let (release, wait) = mpsc::channel();
    let (output, received) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        stream_output(
            1,
            Echo {
                release: wait,
                sent: false,
            },
            &flow,
            &output,
            OUTPUT_BLOCK,
        )
    });
    let result = received.recv_timeout(Duration::from_secs(2));
    release.send(()).unwrap();
    worker.join().unwrap().unwrap();
    let Reply::ProcessData { data, .. } = result.unwrap().result.unwrap() else {
        panic!()
    };
    assert_eq!(URL_SAFE_NO_PAD.decode(data).unwrap(), b"echo");
}

#[test]
fn legacy_spawn_uses_the_small_window_and_oversized_windows_are_rejected() {
    let request: Request =
        serde_json::from_value(serde_json::json!({"id": 1, "command": {"Spawn": {
            "process": 2, "program": "shell", "args": [], "cwd": "", "terminal": [80, 24]
        }}}))
        .unwrap();
    assert!(matches!(
        request.command,
        Command::Spawn {
            output_window: 0,
            ..
        }
    ));
    let (output, _) = mpsc::sync_channel(1);
    let mut manager = Manager::new(output, Ownership::default());
    assert!(
        manager
            .spawn(
                2,
                "shell",
                &[],
                Path::new("."),
                Some((80, 24)),
                OUTPUT_WINDOW + 1
            )
            .unwrap_err()
            .to_string()
            .contains("output window exceeds limit")
    );
}
