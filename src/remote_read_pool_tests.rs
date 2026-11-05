use super::*;
use crate::{
    background::CancellationToken,
    remote::{Command, RemotePath, Reply, Request, text},
};
use std::time::{Duration, Instant};

fn path(value: &str) -> RemotePath {
    RemotePath::new(value.as_bytes().to_vec()).unwrap()
}
fn fixture() -> (
    tempfile::TempDir,
    Arc<Mutex<text::Store>>,
    ReadPool,
    mpsc::Receiver<Response>,
) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("slow.rs"),
        "x".repeat(text::INLINE_BYTES + 1),
    )
    .unwrap();
    std::fs::write(root.path().join("fast.rs"), "fast").unwrap();
    std::fs::create_dir(root.path().join("directory")).unwrap();
    let store = Arc::new(Mutex::new(text::Store::default()));
    let documents = Arc::default();
    let (output, responses) = mpsc::sync_channel(32);
    let pool = ReadPool::new(
        || {
            let mut service = Service::new(root.path()).unwrap();
            service.text = store.clone();
            service.documents = Arc::clone(&documents);
            service
        },
        output,
        Arc::default(),
    );
    (root, store, pool, responses)
}

fn enqueue(pool: &mut ReadPool, id: u64, command: Command, cancel: &CancellationToken) -> bool {
    pool.cancels.lock().unwrap().insert(id, cancel.clone());
    let result = pool.get().unwrap().enqueue(
        Request {
            id,
            workspace: None,
            command,
        },
        Some(cancel.clone()),
    );
    if result.is_err() {
        pool.cancels.lock().unwrap().remove(&id);
    }
    result.is_ok()
}
fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "read worker failed to reach the expected state"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn reply(responses: &mpsc::Receiver<Response>) -> Response {
    responses
        .recv_timeout(Duration::from_secs(3))
        .expect("an independent read was pinned behind a stalled read")
}

#[test]
fn optional_read_workers_stay_lazy_and_sequential_reads_reuse_one_thread() {
    let (_root, _store, mut pool, responses) = fixture();
    assert!(pool.workers.is_empty());
    for id in 1..=5 {
        assert!(enqueue(
            &mut pool,
            id,
            Command::Open {
                path: path("fast.rs")
            },
            &CancellationToken::new()
        ));
        assert!(matches!(reply(&responses).result, Ok(Reply::Opened { .. })));
        wait_for(|| pool.load.pending.load(Ordering::Relaxed) == 0);
        assert_eq!(
            pool.workers.len(),
            1,
            "a sequential session does not start a spare thread"
        );
    }
    assert!(pool.cancels.lock().unwrap().is_empty());
}

#[test]
fn shared_inbox_keeps_later_opens_reloads_and_directory_opens_off_a_stalled_worker() {
    let (_root, store, mut pool, responses) = fixture();
    let held = store.lock().unwrap();
    assert!(enqueue(
        &mut pool,
        1,
        Command::Open {
            path: path("slow.rs")
        },
        &CancellationToken::new()
    ));
    for (id, command) in [
        (
            2,
            Command::Open {
                path: path("fast.rs"),
            },
        ),
        (
            3,
            Command::Reload {
                path: path("fast.rs"),
                known: None,
                read_changed: true,
            },
        ),
        (
            4,
            Command::OpenLocation {
                path: path("directory"),
            },
        ),
    ] {
        assert!(enqueue(&mut pool, id, command, &CancellationToken::new()));
    }
    let mut completed = Vec::new();
    for _ in 0..3 {
        let response = reply(&responses);
        assert_ne!(response.id, 1, "the first read is still stalled");
        match response.id {
            2 | 3 => {
                assert!(matches!(response.result, Ok(Reply::Opened { text, .. }) if text == "fast"))
            }
            4 => assert!(matches!(response.result, Ok(Reply::Directory { .. }))),
            id => panic!("unexpected reply {id}"),
        }
        completed.push(response.id);
    }
    completed.sort_unstable();
    assert_eq!(completed, [2, 3, 4]);
    assert_eq!(pool.workers.len(), 2);
    drop(held);
    let response = reply(&responses);
    assert_eq!(response.id, 1);
    let Ok(Reply::Text(text::Reply::Opened { transfer, .. })) = response.result else {
        panic!("large file reply");
    };
    store.lock().unwrap().close(transfer);
    wait_for(|| pool.load.pending.load(Ordering::Relaxed) == 0);
}

#[test]
fn saturated_reads_are_bounded_and_cancellation_releases_late_transfer_handles() {
    let (_root, store, mut pool, responses) = fixture();
    let documents = pool.services[0].as_ref().unwrap().documents.clone();
    let held = documents.lock().unwrap();
    let tokens: Vec<_> = (0..12).map(|_| CancellationToken::new()).collect();
    for (index, token) in tokens[..2].iter().enumerate() {
        assert!(enqueue(
            &mut pool,
            index as u64 + 1,
            Command::Open {
                path: path("slow.rs")
            },
            token
        ));
    }
    // Both reads have allocated leases and are stalled publishing their saved
    // checkpoints, beyond the last cancellable file-read/hash operation.
    wait_for(|| store.lock().unwrap().live_transfers() == 2);
    for (index, token) in tokens[2..10].iter().enumerate() {
        assert!(enqueue(
            &mut pool,
            index as u64 + 3,
            Command::Reload {
                path: path("missing.rs"),
                known: Some(crate::remote::Revision([0; 32])),
                read_changed: true,
            },
            token
        ));
    }
    assert!(
        !enqueue(
            &mut pool,
            13,
            Command::Reload {
                path: path("missing.rs"),
                known: Some(crate::remote::Revision([0; 32])),
                read_changed: true,
            },
            &CancellationToken::new()
        ),
        "background work cannot consume the reserved open slots"
    );
    for (index, token) in tokens[10..].iter().enumerate() {
        assert!(enqueue(
            &mut pool,
            index as u64 + 11,
            Command::Open {
                path: path("missing.rs")
            },
            token
        ));
    }
    assert!(!enqueue(
        &mut pool,
        14,
        Command::Open {
            path: path("missing.rs")
        },
        &CancellationToken::new()
    ));
    assert_eq!(pool.workers.len(), 2);
    assert_eq!(
        pool.load.pending.load(Ordering::Relaxed),
        12,
        "two running, eight ordinary and two reserved opens, even after rejection"
    );
    for token in &tokens {
        token.cancel();
    }
    drop(held);
    for _ in 0..12 {
        let response = reply(&responses);
        assert!(
            response.result.is_err(),
            "cancelled reads cannot publish a file or a new transfer"
        );
        assert_eq!(
            response.result.unwrap_err().into_io().kind(),
            std::io::ErrorKind::Interrupted
        );
    }
    wait_for(|| pool.load.pending.load(Ordering::Relaxed) == 0);
    assert_eq!(store.lock().unwrap().live_transfers(), 0);
    assert!(pool.cancels.lock().unwrap().is_empty());
    assert!(enqueue(
        &mut pool,
        15,
        Command::Open {
            path: path("fast.rs")
        },
        &CancellationToken::new()
    ));
    assert!(matches!(reply(&responses).result, Ok(Reply::Opened { .. })));
}

fn message(id: u64, foreground: bool) -> ActorMessage {
    Some((
        id,
        (
            Request {
                id,
                workspace: None,
                command: if foreground {
                    Command::Open {
                        path: path("fast.rs"),
                    }
                } else {
                    Command::Reload {
                        path: path("fast.rs"),
                        known: Some(crate::remote::Revision([0; 32])),
                        read_changed: true,
                    }
                },
            },
            Some(CancellationToken::new()),
        ),
    ))
}

#[test]
fn explicit_opens_overtake_buffered_reloads_and_background_work_gets_a_fair_turn() {
    let (sender, receiver) = mpsc::sync_channel(10);
    let mut requests = SharedInbox::new(receiver);
    for id in 1..=8 {
        sender.send(message(id, false)).unwrap();
    }
    sender.send(message(90, true)).unwrap();
    assert_eq!(requests.next().unwrap().unwrap().0, 90);
    // Most reloads are now buffered outside the channel. A newly arrived
    // open must still jump ahead of them, not wait for that buffer to empty.
    sender.send(message(91, true)).unwrap();
    assert_eq!(requests.next().unwrap().unwrap().0, 91);
    for id in 1..=8 {
        assert_eq!(requests.next().unwrap().unwrap().0, id);
    }
    sender.send(message(100, false)).unwrap();
    for id in 101..=109 {
        sender.send(message(id, true)).unwrap();
    }
    for id in 101..=108 {
        assert_eq!(requests.next().unwrap().unwrap().0, id);
    }
    assert_eq!(
        requests.next().unwrap().unwrap().0,
        100,
        "eight opens cannot starve a waiting reload"
    );
    assert_eq!(requests.next().unwrap().unwrap().0, 109);
}

#[test]
fn explicit_opens_keep_reserved_capacity_before_workers_begin_receiving() {
    let (_root, _store, mut pool, responses) = fixture();
    let inbox = pool.requests.0.clone();
    let held = inbox.lock().unwrap();
    for id in 1..=10 {
        assert!(enqueue(
            &mut pool,
            id,
            Command::Reload {
                path: path("missing.rs"),
                known: Some(crate::remote::Revision([0; 32])),
                read_changed: true,
            },
            &CancellationToken::new()
        ));
    }
    for id in 11..=12 {
        assert!(
            enqueue(
                &mut pool,
                id,
                Command::Open {
                    path: path("fast.rs")
                },
                &CancellationToken::new()
            ),
            "reloads must leave capacity for explicit opens even before workers receive"
        );
    }
    assert!(!enqueue(
        &mut pool,
        13,
        Command::Open {
            path: path("fast.rs")
        },
        &CancellationToken::new()
    ));
    drop(held);
    let mut opened = Vec::new();
    for _ in 0..12 {
        let response = reply(&responses);
        if matches!(response.id, 11 | 12) {
            assert!(matches!(response.result, Ok(Reply::Opened { text, .. }) if text == "fast"));
            opened.push(response.id);
        } else {
            assert!(response.result.is_err());
        }
    }
    opened.sort_unstable();
    assert_eq!(opened, [11, 12]);
    wait_for(|| pool.load.pending.load(Ordering::Relaxed) == 0);
    assert!(pool.cancels.lock().unwrap().is_empty());
}

#[test]
fn connection_teardown_does_not_wait_for_stalled_read_workers() {
    let (_root, store, mut pool, responses) = fixture();
    let documents = pool.services[0].as_ref().unwrap().documents.clone();
    let held = documents.lock().unwrap();
    let token = CancellationToken::new();
    assert!(enqueue(
        &mut pool,
        1,
        Command::Open {
            path: path("slow.rs")
        },
        &token
    ));
    wait_for(|| store.lock().unwrap().live_transfers() == 1);
    let load = pool.load.clone();
    token.cancel();
    let started = Instant::now();
    drop(pool);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "disconnect must not join a stalled filesystem thread"
    );
    drop(held);
    assert!(reply(&responses).result.is_err());
    wait_for(|| load.pending.load(Ordering::Relaxed) == 0);
    assert_eq!(store.lock().unwrap().live_transfers(), 0);
}
