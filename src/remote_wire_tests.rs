use super::*;
use std::{hint::black_box, time::Instant};

#[test]
fn frame_prefix_matches_the_advertised_protocol_version() {
    assert_eq!(PREFIX, format!("VTCR{VERSION}"));
}

#[test]
fn search_output_discards_obsolete_batches_and_finishes_partial_frames() {
    let hit = |label: &str| search::Hit { path: RemotePath::new(b"file.txt".to_vec()).unwrap(),
        line: 1, start: 0, end: 1, text: label.into() };
    let mut queue = OutputQueue::default();
    queue.push(Response { id: 0, result: Ok(Reply::SearchProgress { request: 7, hits: vec![hit("started")] }) }).unwrap();
    let packet = queue.packets.front_mut().unwrap();
    // An incompressible packet forces the normal multi-frame/reassembly path.
    packet.data = encode(&Response { id: 0, result: Ok(Reply::SearchProgress { request: 7,
        hits: vec![hit(&(0..20_000).map(|i| format!("{i:x}")).collect::<String>())] }) }).unwrap();
    queue.bytes = packet.data.len();
    let mut bytes = Vec::new();
    queue.step(&mut bytes, 42).unwrap();
    assert!(queue.packets.front().unwrap().offset > 0);
    queue.push(Response { id: 0, result: Ok(Reply::SearchProgress { request: 7, hits: vec![hit("obsolete")] }) }).unwrap();
    queue.push(Response { id: 7, result: Ok(Reply::Search { hits: Vec::new(), truncated: false }) }).unwrap();
    queue.push(Response { id: 0, result: Ok(Reply::SearchProgress { request: 8, hits: vec![hit("current")] }) }).unwrap();
    queue.discard_obsolete_search(8).unwrap();
    while !queue.is_empty() { queue.step(&mut bytes, 42).unwrap(); }
    let mut decoder = Decoder::new('S', 42);
    let mut current = false;
    let mut partial = false;
    let mut cancelled = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if let Some((_, data)) = decoder.line(line).unwrap() {
            let reply: Response = serde_json::from_slice(&data).unwrap();
            match reply.result {
                Ok(Reply::SearchProgress { request: 8, hits }) => { assert_eq!(&*hits[0].text, "current"); current = true; }
                Ok(Reply::SearchProgress { request: 7, hits }) => { assert_ne!(&*hits[0].text, "obsolete"); partial = true; }
                Err(error) => { assert_eq!(reply.id, 7); assert_eq!(error.into_io().kind(), io::ErrorKind::Interrupted); cancelled = true; }
                other => panic!("unexpected reply: {other:?}"),
            }
        }
    }
    assert!(current && partial && cancelled);
    assert_eq!(queue.bytes, 0);
}

#[test]
fn search_progress_queue_errors_keep_the_request_id_in_the_wire_frame() {
    let mut queue = OutputQueue { bytes: MAX_MESSAGE * 2, ..OutputQueue::default() };
    queue.push(Response { id: 0, result: Ok(Reply::SearchProgress { request: 7, hits: Vec::new() }) }).unwrap();
    let mut bytes = Vec::new();
    queue.step(&mut bytes, 42).unwrap();
    let (id, data) = Decoder::new('S', 42).line(&bytes).unwrap().unwrap();
    let response: Response = serde_json::from_slice(&data).unwrap();
    assert_eq!(id, 7);
    assert_eq!(response.id, id);
    assert!(response.result.unwrap_err().to_string().contains("queue is full"));
}

#[test]
fn search_progress_batches_finish_before_their_terminal_reply_without_blocking_other_requests() {
    let mut entropy = 7u64;
    let text = (0..40_000).map(|_| {
        entropy ^= entropy << 13;
        entropy ^= entropy >> 7;
        entropy ^= entropy << 17;
        char::from(b' ' + (entropy % 95) as u8)
    }).collect::<String>();
    let hit = |text: String| search::Hit { path: RemotePath::new(b"file.txt".to_vec()).unwrap(),
        line: 1, start: 0, end: 1, text: text.into() };
    let mut queue = OutputQueue::default();
    queue.push(Response { id: 0, result: Ok(Reply::SearchProgress { request: 7, hits: vec![hit(text)] }) }).unwrap();
    let mut bytes = Vec::new();
    queue.step(&mut bytes, 42).unwrap();
    assert!(!queue.is_empty(), "exercise a partially written progress event");
    queue.push(Response { id: 0, result: Ok(Reply::SearchProgress { request: 7, hits: vec![hit("second".into())] }) }).unwrap();
    queue.push(Response { id: 7, result: Ok(Reply::Search { hits: Vec::new(), truncated: false }) }).unwrap();
    queue.push(Response { id: 8, result: Ok(Reply::Done) }).unwrap();
    while !queue.is_empty() { queue.step(&mut bytes, 42).unwrap(); }
    let mut decoder = Decoder::new('S', 42);
    let mut received = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if let Some((_, data)) = decoder.line(line).unwrap() {
            let response: Response = serde_json::from_slice(&data).unwrap();
            received.push(match response.result.unwrap() {
                Reply::SearchProgress { hits, .. } => if hits[0].text.len() > 100 { "first" } else { "second" },
                Reply::Search { .. } => "done",
                Reply::Done => "other request",
                _ => panic!("unexpected reply"),
            });
        }
    }
    assert_eq!(received, ["other request", "first", "second", "done"]);
}

#[test]
fn terminal_output_keeps_order_and_eof_while_another_terminal_can_echo() {
    let mut entropy = 23u64;
    let output: Vec<u8> = (0..32 * 1024).map(|_| {
        entropy ^= entropy << 13; entropy ^= entropy >> 7; entropy ^= entropy << 17;
        entropy as u8
    }).collect();
    let mut queue = OutputQueue::default();
    queue.push(Response { id: 0, result: Ok(Reply::SearchProgress { request: 7, hits: Vec::new() }) }).unwrap();
    for (process, bytes) in [(7, output.as_slice()), (7, b"tail"), (8, b"echo")] {
        queue.push(Response { id: 0, result: Ok(Reply::ProcessData { process, data: URL_SAFE_NO_PAD.encode(bytes) }) }).unwrap();
    }
    queue.push(Response { id: 0, result: Ok(Reply::ProcessEnd { process: 7, error: None }) }).unwrap();
    // Request and process IDs use separate ordering domains.
    queue.push(Response { id: 7, result: Ok(Reply::Done) }).unwrap();
    let mut wire = Vec::new();
    queue.step(&mut wire, 42).unwrap();
    let (_, first) = Decoder::new('S', 42).line(&wire).unwrap().unwrap();
    assert!(matches!(serde_json::from_slice::<Response>(&first).unwrap().result,
        Ok(Reply::ProcessData { process: 8, .. })));
    queue.step(&mut wire, 42).unwrap();
    assert!(queue.packets.iter().any(|packet| packet.offset != 0));
    while !queue.is_empty() { queue.step(&mut wire, 42).unwrap(); }
    let mut decoder = Decoder::new('S', 42);
    let mut received = Vec::new();
    let mut actual = Vec::new();
    for line in wire.split_inclusive(|byte| *byte == b'\n') {
        if let Some((_, bytes)) = decoder.line(line).unwrap() {
            let response: Response = serde_json::from_slice(&bytes).unwrap();
            match response.result.unwrap() {
                Reply::ProcessData { process: 7, data } => {
                    let bytes = URL_SAFE_NO_PAD.decode(data).unwrap();
                    received.push(if bytes.len() > 4 { "bulk" } else { "tail" });
                    actual.extend(bytes);
                }
                Reply::ProcessData { process: 8, data } => {
                    assert_eq!(URL_SAFE_NO_PAD.decode(data).unwrap(), b"echo");
                    received.push("echo");
                }
                Reply::ProcessEnd { process: 7, error: None } => received.push("end"),
                Reply::SearchProgress { .. } => received.push("search"),
                Reply::Done => received.push("request"),
                other => panic!("unexpected reply: {other:?}"),
            }
        }
    }
    assert_eq!(received.iter().copied().filter(|item| !matches!(*item, "search" | "request")).collect::<Vec<_>>(),
        ["echo", "bulk", "tail", "end"]);
    assert!(received.contains(&"search") && received.contains(&"request"));
    assert_eq!(&actual[..output.len()], output);
    assert_eq!(&actual[output.len()..], b"tail");
    assert_eq!(queue.bytes, 0);
}

#[test]
fn continuous_terminal_output_cannot_starve_workspace_replies() {
    let mut queue = OutputQueue::default();
    queue.push(Response { id: 7, result: Ok(Reply::Done) }).unwrap();
    let mut decoder = Decoder::new('S', 42);
    let mut completed = false;
    for _ in 0..9 {
        queue.push(Response { id: 0, result: Ok(Reply::ProcessData { process: 1, data: URL_SAFE_NO_PAD.encode(b"output") }) }).unwrap();
        let mut bytes = Vec::new(); queue.step(&mut bytes, 42).unwrap();
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if let Some((id, _)) = decoder.line(line).unwrap() { completed |= id == 7; }
        }
    }
    assert!(completed, "workspace traffic must make progress during a terminal flood");
}

#[derive(Default)]
struct Writes {
    bytes: Vec<u8>,
    calls: usize,
}
impl Write for Writes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn completed_single_frames_do_not_need_a_fragment_assembly_slot() {
    let mut decoder = Decoder::new('C', 42);
    for id in 1..=MAX_PENDING as u64 {
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            'C',
            42,
            id,
            0,
            &vec![0; CHUNK],
            false,
            &mut String::new(),
        )
        .unwrap();
        assert!(decoder.line(&bytes).unwrap().is_none());
    }
    // A small cancellation acknowledgement or error must still be deliverable
    // while every *incomplete* message has an assembly slot.
    let mut bytes = Vec::new();
    write_message(&mut bytes, 'C', 42, 99, &encode(&"done").unwrap()).unwrap();
    let (id, data) = decoder.line(&bytes).unwrap().unwrap();
    assert_eq!(id, 99);
    assert_eq!(serde_json::from_slice::<String>(&data).unwrap(), "done");
    assert_eq!(decoder.pending.len(), MAX_PENDING);
    assert_eq!(decoder.bytes, MAX_PENDING * CHUNK);
    // Reusing a partially assembled ID must still fail, even for a final frame.
    bytes.clear();
    write_message(&mut bytes, 'C', 42, 1, &encode(&"wrong").unwrap()).unwrap();
    assert!(decoder.line(&bytes).is_err());
}

#[test]
fn frame_writer_is_atomic_and_handles_partial_writes_and_pty_wrappers() {
    for direction in ['C', 'S'] {
        for size in [1, CHUNK - 1, CHUNK, CHUNK + 1, CHUNK * 3 + 1] {
            let mut data = vec![0];
            data.extend((0..size).map(|i| (i % 251) as u8));
            let mut writer = Writes::default();
            write_message(&mut writer, direction, u64::MAX, u64::MAX, &data).unwrap();
            let frames = data.len().div_ceil(CHUNK);
            assert_eq!(
                writer.calls, frames,
                "one write per frame, {direction}/{size}"
            );
            struct Partial<'a>(&'a mut Vec<u8>);
            impl Write for Partial<'_> {
                fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                    let size = bytes.len().min(7);
                    self.0.extend_from_slice(&bytes[..size]);
                    Ok(size)
                }
                fn flush(&mut self) -> io::Result<()> {
                    Ok(())
                }
            }
            let mut partial = Vec::new();
            write_message(
                &mut Partial(&mut partial),
                direction,
                u64::MAX,
                u64::MAX,
                &data,
            )
            .unwrap();
            assert_eq!(writer.bytes, partial);
            let mut decoder = Decoder::new(direction, u64::MAX);
            let mut result = None;
            for line in writer.bytes.split_inclusive(|byte| *byte == b'\n') {
                assert!(line.len() < 4096, "canonical PTY line budget");
                let mut wrapped = b"shell prefix\x1b[0m".to_vec();
                wrapped.extend_from_slice(line);
                // Shell/ConPTY text may precede a server OSC record.
                result = decoder
                    .line(if direction == 'S' { &wrapped } else { line })
                    .unwrap();
            }
            assert_eq!(result.unwrap(), (u64::MAX, data[1..].to_vec()));
        }
    }
}

#[test]
fn server_envelope_after_client_echo_is_not_mistaken_for_the_echo() {
    let mut bytes = format!("{PREFIX}C:echoed-client-text ").into_bytes();
    write_message(&mut bytes, 'S', 42, 7, &encode(&"answer").unwrap()).unwrap();
    let mut decoder = Decoder::new('S', 42);
    let (id, data) = decoder.line(&bytes).unwrap().unwrap();
    assert_eq!(id, 7);
    assert_eq!(serde_json::from_slice::<String>(&data).unwrap(), "answer");
}

#[test]
#[ignore = "Transport CPU benchmark; run explicitly on an otherwise idle core"]
fn transport_cpu_benchmark() {
    // Fixed entropy prevents an unrealistically compressible all-identical
    // document from hiding framing costs. The inputs and iteration counts stay
    // identical across before/after runs.
    let tiny = encode(&serde_json::json!({"id": 7, "result": {"Saved": {
        "revision": "f1a9f613c52a0e57", "identity": "example.py", "size": 900,
        "modified": 123456789
    }}}))
    .unwrap();
    let mut entropy = 7u64;
    let mut bulk = vec![0];
    bulk.extend((0..128 * 1024).map(|_| {
        entropy ^= entropy << 13;
        entropy ^= entropy >> 7;
        entropy ^= entropy << 17;
        b' ' + (entropy % 95) as u8
    }));
    for (label, data, iterations) in [("small", tiny, 30_000), ("bulk", bulk, 200)] {
        let mut frame = Vec::new();
        write_message(&mut frame, 'S', 42, 7, &data).unwrap();
        let lines: Vec<_> = frame.split_inclusive(|byte| *byte == b'\n').collect();
        let mut decoder = Decoder::new('S', 42);
        let start = Instant::now();
        for _ in 0..iterations {
            for line in &lines {
                black_box(decoder.line(black_box(line)).unwrap());
            }
        }
        let decode = start.elapsed().as_secs_f64() * 1e9 / iterations as f64;
        let mut writer = Writes::default();
        writer.bytes.reserve(frame.len());
        let start = Instant::now();
        for _ in 0..iterations {
            writer.bytes.clear();
            write_message(black_box(&mut writer), 'S', 42, 7, black_box(&data)).unwrap();
        }
        let write = start.elapsed().as_secs_f64() * 1e9 / iterations as f64;
        println!(
            "WIRE_BENCH {{\"label\":\"{label}\",\"bytes\":{},\"frames\":{},\"iterations\":{iterations},\"decode_ns\":{decode:.1},\"write_ns\":{write:.1},\"writes_per_message\":{}}}",
            data.len(),
            lines.len(),
            writer.calls / iterations
        );
    }
}

#[test]
fn diagnostics_observer_only_sees_non_protocol_output() {
    let mut decoder = Decoder::new('S', 42);
    let mut data = vec![0]; data.extend((0..10_000).map(|n| (n % 251) as u8));
    let mut frames = Vec::new(); write_message(&mut frames, 'S', 42, 7, &data).unwrap();
    let mut seen = Vec::new(); let mut complete = None;
    for line in frames.split_inclusive(|b| *b == b'\n') {
        if let Some(message) = decoder.line_with_output(line, |output| seen.push(output.to_vec())).unwrap() { complete = Some(message); }
    }
    assert_eq!(complete.unwrap().1, data[1..]);
    let mut echo = Vec::new(); write_message(&mut echo, 'C', 42, 8, &[0, 1, 2, 3]).unwrap();
    for line in echo.split_inclusive(|b| *b == b'\n') { decoder.line_with_output(line, |output| seen.push(output.to_vec())).unwrap(); }
    let broken = format!("{PREFIX}S:000000000000002a:broken\n");
    assert!(decoder.line_with_output(broken.as_bytes(), |output| seen.push(output.to_vec())).is_err());
    assert!(seen.is_empty(), "No frame payload may reach diagnostics");
    decoder.line_with_output(b"ssh: Permission denied (publickey).\n", |output| seen.push(output.to_vec())).unwrap();
    assert_eq!(seen, [b"ssh: Permission denied (publickey).\n".to_vec()]);
}
