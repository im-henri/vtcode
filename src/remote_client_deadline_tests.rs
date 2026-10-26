use super::*;

fn silent_client(root: &std::path::Path) -> (Client, mpsc::Sender<()>) {
    struct Silent(mpsc::Receiver<()>);
    impl Read for Silent {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Ok(0)
        }
    }
    let hello = super::super::service::Service::new(root).unwrap().hello(42);
    let (end, input) = mpsc::channel();
    (Client::attach(BufReader::new(Silent(input)), Vec::new(), hello).unwrap(), end)
}

#[test]
fn stalled_control_writes_do_not_hold_ui_mailbox_locks() {
    struct StalledWrite {
        entered: Option<mpsc::SyncSender<()>>,
        release: mpsc::Receiver<()>,
    }
    impl Write for StalledWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(entered) = self.entered.take() {
                entered.send(()).unwrap();
                self.release.recv().unwrap();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let root = tempfile::tempdir().unwrap();
    let client = super::super::tests::connection(root.path(), Arc::default(), Arc::default());
    client.call(Command::Ping).unwrap();
    let mut blocked = Vec::new();
    for kind in 0..9 {
        let shared = client.0.shared.clone();
        match kind {
            0 => { shared.agent_closed.lock().unwrap().insert(1); }
            1 => { shared.agent_credits.lock().unwrap().insert((1, 2)); }
            2 => { shared.agent_histories_closed.lock().unwrap().insert(1, 1); }
            3 => { shared.rollback_closed.lock().unwrap().insert(1); }
            4 => { shared.text_closed.lock().unwrap().insert(1); }
            5 => { shared.binary_closed.lock().unwrap().insert(1); }
            6 => { shared.cancelled.lock().unwrap().insert(1); }
            7 => { shared.resized.lock().unwrap().insert(1, (80, 24)); }
            _ => { shared.agent_cancelled.lock().unwrap().insert(
                (1, vtcode::acp::protocol::SessionId::new("session")),
                Request { id: 0, workspace: None, command: Command::Cancel { request: 1 } }); }
        }
        let (entered, ready) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let worker = std::thread::spawn(move || {
            let (_, controls) = mpsc::sync_channel(1);
            write_controls(&mut StalledWrite { entered: Some(entered), release: gate },
                &worker_shared, &controls, 42).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let free = match kind {
            0 => shared.agent_closed.try_lock().is_ok(),
            1 => shared.agent_credits.try_lock().is_ok(),
            2 => shared.agent_histories_closed.try_lock().is_ok(),
            3 => shared.rollback_closed.try_lock().is_ok(),
            4 => shared.text_closed.try_lock().is_ok(),
            5 => shared.binary_closed.try_lock().is_ok(),
            6 => shared.cancelled.try_lock().is_ok(),
            7 => shared.resized.try_lock().is_ok(),
            _ => shared.agent_cancelled.try_lock().is_ok(),
        };
        release.send(()).unwrap();
        worker.join().unwrap();
        if !free { blocked.push(kind); }
    }
    client.call(Command::Shutdown).unwrap();
    assert!(blocked.is_empty(), "Network writes held UI mailbox locks: {blocked:?}");
}

#[test]
fn silent_peer_failure_releases_file_waiters_capacity_and_transfer_permits() {
    let root = tempfile::tempdir().unwrap();
    let (client, _end) = silent_client(root.path());
    let command = || Command::Open { path: super::super::RemotePath::new(b"file.rs".to_vec()).unwrap() };
    let mut replies = Vec::new();
    for _ in 0..8 { replies.push(client.submit_inner(command(), None).unwrap().1); }
    let first = client.text_permit().unwrap();
    let second = client.text_permit().unwrap();
    let (done, results) = mpsc::channel();
    let file_client = client.clone();
    let file_done = done.clone();
    let file = std::thread::spawn(move || file_done.send(file_client.call_file(command()).is_err()).unwrap());
    let permit_client = client.clone();
    let permit = std::thread::spawn(move || done.send(permit_client.text_permit().is_err()).unwrap());
    liveness::expire_silent_peer_for_test(Arc::downgrade(&client.0.shared), Arc::downgrade(&client.0.sender)).unwrap();
    for reply in replies {
        assert!(reply.recv_timeout(Duration::from_secs(5)).unwrap().take().is_err());
    }
    for _ in 0..2 { assert!(results.recv_timeout(Duration::from_secs(5)).unwrap()); }
    file.join().unwrap();
    permit.join().unwrap();
    assert!(!client.connected());
    assert!(client.0.shared.pending.lock().unwrap().is_empty());
    // Repeated teardown is harmless and never revives a request/permit.
    client.disconnect();
    drop((first, second));
    assert!(client.text_permit().is_err());
}

#[test]
fn disconnect_cannot_lose_the_notification_between_a_permit_check_and_wait() {
    let root = tempfile::tempdir().unwrap();
    let (client, _end) = silent_client(root.path());
    let shared = client.0.shared.clone();
    let active = shared.text_active.lock().unwrap();
    let (finished, done) = mpsc::sync_channel(1);
    let failing = shared.clone();
    let worker = std::thread::spawn(move || {
        failing.fail("Disconnected during permit acquisition".into());
        finished.send(()).unwrap();
    });
    // Model a waiter paused after checking the closed predicate while it still
    // holds the permit mutex. Failure must synchronize before notifying it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !shared.closed.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
    let premature = done.recv_timeout(Duration::from_millis(20)).is_ok();
    let (active, result) = shared.text_ready.wait_timeout(active, Duration::from_secs(2)).unwrap();
    drop(active);
    worker.join().unwrap();
    assert!(!premature && !result.timed_out(), "Disconnect notification raced the permit waiter");
}

#[test]
fn workspace_replace_mutation_timeout_is_not_an_acknowledged_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let client = super::super::tests::connection(root.path(), Arc::default(), Arc::default());
    for mutation in [false, true] {
        let cancel = crate::background::CancellationToken::new();
        cancel.cancel();
        // Keep the reply sender alive: cancellation has been sent, but no peer
        // acknowledgement says whether the mutation finished or was stopped.
        let (_sender, receiver) = mpsc::sync_channel::<Delivery>(1);
        let result = client.wait_progress(
            9999,
            receiver,
            &cancel,
            mutation,
            Duration::from_millis(1),
            &mut |_| {},
        );
        assert_eq!(
            result.unwrap_err().kind(),
            if mutation {
                io::ErrorKind::TimedOut
            } else {
                io::ErrorKind::Interrupted
            }
        );
    }
    client.call(Command::Ping).unwrap();
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn workspace_replace_long_mutations_wait_for_the_final_ack_and_get_a_fresh_cancel_deadline() {
    let root = tempfile::tempdir().unwrap();
    let client = super::super::tests::connection(root.path(), Arc::default(), Arc::default());
    for cancel_during_work in [false, true] {
        let cancel = crate::background::CancellationToken::new();
        let (sender, receiver) = mpsc::sync_channel(1);
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let peer_barrier = barrier.clone();
        let peer = std::thread::spawn(move || {
            peer_barrier.wait();
            std::thread::sleep(Duration::from_millis(80));
            let _ = sender.send(Delivery {
                result: Some(Ok(Reply::Done)),
                cleanup: None,
            });
        });
        client
            .0
            .shared
            .deletion_progress
            .lock()
            .unwrap()
            .insert(9998, Default::default());
        barrier.wait();
        let started = std::time::Instant::now();
        let result = client.wait_progress(
            9998,
            receiver,
            &cancel,
            true,
            Duration::from_millis(40),
            &mut |_| {
                if cancel_during_work && started.elapsed() >= Duration::from_millis(40) {
                    cancel.cancel();
                }
            },
        );
        peer.join().unwrap();
        client
            .0
            .shared
            .deletion_progress
            .lock()
            .unwrap()
            .remove(&9998);
        assert!(
            matches!(result, Ok(Reply::Done)),
            "cancel={cancel_during_work}: {result:?}"
        );
        assert_eq!(cancel.is_cancelled(), cancel_during_work);
    }
    client.call(Command::Ping).unwrap();
    client.call(Command::Shutdown).unwrap();
}
