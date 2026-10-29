use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[path = "remote_git_untracked_tests.rs"]
mod git_untracked;

#[test]
fn a_blocked_open_cannot_delay_an_unrelated_save_or_publish_another_roots_checkpoint() {
    use std::io::{BufRead, Write};
    use std::net::{TcpListener, TcpStream};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("large.rs"),
        "x".repeat(super::super::text::INLINE_BYTES + 1),
    )
    .unwrap();
    std::fs::write(root.path().join("small.rs"), "original\n").unwrap();
    let mut service = Service::new(root.path()).unwrap();
    let Reply::Opened { revision, .. } = service
        .execute(Command::Open {
            path: path("small.rs"),
        })
        .unwrap()
    else {
        panic!();
    };
    let text_store = service.text.clone();
    let held = text_store.lock().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.set_nodelay(true).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nodelay(true).unwrap();
    let worker =
        std::thread::spawn(move || serve_service(service, server.try_clone().unwrap(), server));
    let mut reader = std::io::BufReader::new(client.try_clone().unwrap());
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).unwrap();
    let hello = client::Client::parse_hello(&line).unwrap().unwrap();
    let mut batch = Vec::new();
    for (id, command) in [
        (
            1,
            Command::Open {
                path: path("large.rs"),
            },
        ),
        (
            2,
            Command::Save {
                path: path("small.rs"),
                base: Some(revision),
                encoding: "UTF-8".into(),
                change: Change {
                    start: 0,
                    end: 0,
                    text: "saved\n".into(),
                },
            },
        ),
        (3, Command::PerformanceSnapshot),
    ] {
        wire::write_message(
            &mut batch,
            'C',
            hello.session,
            id,
            &wire::encode(&Request {
                id,
                workspace: None,
                command,
            })
            .unwrap(),
        )
        .unwrap();
    }
    client.write_all(&batch).unwrap();
    let result = (|| -> io::Result<Vec<Response>> {
        let mut decoder = wire::Decoder::new('S', hello.session);
        let mut responses = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if let Some((_, data)) = decoder.line(&line)? {
                responses.push(serde_json::from_slice(&data)?);
                if responses.len() == 2 { return Ok(responses); }
            }
        }
    })();
    drop(held); // unblock on regression too, before joining the owned service
    client.shutdown(std::net::Shutdown::Both).unwrap();
    let _ = worker.join().unwrap();
    let responses = result.expect("save and diagnostics complete while the open is still blocked");
    assert!(responses.iter().any(|response| response.id == 2 && matches!(response.result, Ok(Reply::Saved { .. }))));
    assert!(responses.iter().any(|response| response.id == 3 && matches!(response.result, Ok(Reply::PerformanceSnapshot(_)))));
    assert_eq!(
        std::fs::read_to_string(root.path().join("small.rs")).unwrap(),
        "saved\noriginal\n"
    );

    // A delayed read may replace the small LRU's root, but its checkpoint must
    // never be used for another workspace with the same relative filename.
    let other = tempfile::tempdir().unwrap();
    let mut cache = Documents::default();
    let a = Document {
        rope: ropey::Rope::from_str("a"),
        encoding: "UTF-8",
        revision: Revision([0; 32]),
    };
    cache.insert(root.path(), path("same.rs"), a.clone());
    assert!(cache.get(other.path(), &path("same.rs")).is_none());
    cache.remove(other.path(), &path("same.rs"), true);
    assert!(cache.get(root.path(), &path("same.rs")).is_some());
    cache.insert(other.path(), path("same.rs"), a);
    assert!(cache.get(root.path(), &path("same.rs")).is_none());
}

#[test]
fn transfer_cleanup_survives_a_full_file_worker_queue() {
    use std::io::{BufRead, Write};
    use std::net::{TcpListener, TcpStream};
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path()).unwrap();
    let text_store = service.text.clone();
    let transfer = text_store
        .lock()
        .unwrap()
        .read(ropey::Rope::from_str("body"))
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.set_nodelay(true).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nodelay(true).unwrap();
    let worker =
        std::thread::spawn(move || serve_service(service, server.try_clone().unwrap(), server));
    let mut reader = std::io::BufReader::new(client.try_clone().unwrap());
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).unwrap();
    let hello = client::Client::parse_hello(&line).unwrap().unwrap();
    // Hold the store to model an occupied/CPU-starved file worker. Each Read
    // needs this lock; a flood of Close controls must not tear down the session.
    let held = text_store.lock().unwrap();
    let mut batch = Vec::new();
    for id in 1..=9 {
        wire::write_message(
            &mut batch,
            'C',
            hello.session,
            id,
            &wire::encode(&Request {
                id,
                workspace: None,
                command: Command::Text(text::Command::Read {
                    transfer,
                    start: 0,
                    end: 4,
                }),
            })
            .unwrap(),
        )
        .unwrap();
    }
    for _ in 0..16 {
        wire::write_message(
            &mut batch,
            'C',
            hello.session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Close {
                    path: path("loading.txt"),
                },
            })
            .unwrap(),
        )
        .unwrap();
        wire::write_message(
            &mut batch,
            'C',
            hello.session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Text(text::Command::Close { transfer }),
            })
            .unwrap(),
        )
        .unwrap();
    }
    wire::write_message(
        &mut batch,
        'C',
        hello.session,
        99,
        &wire::encode(&Request {
            id: 99,
            workspace: None,
            command: Command::Shutdown,
        })
        .unwrap(),
    )
    .unwrap();
    client.write_all(&batch).unwrap();
    let mut decoder = wire::Decoder::new('S', hello.session);
    let acknowledged = (|| -> io::Result<bool> {
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                return Ok(false);
            }
            if let Some((99, data)) = decoder.line(&line)? {
                let response: Response = serde_json::from_slice(&data)?;
                return Ok(matches!(response.result, Ok(Reply::Done)));
            }
        }
    })();
    drop(held);
    // Join on failure too: no blocked actor/socket may escape the fixture.
    let _ = client.shutdown(std::net::Shutdown::Both);
    let result = worker.join().unwrap();
    assert!(
        acknowledged.unwrap_or(false),
        "Cleanup controls must remain admissible; service: {result:?}"
    );
    assert!(result.is_ok(), "{result:?}");
    assert!(text_store.lock().unwrap().snapshot(transfer).is_err());
}

#[test]
fn resource_cleanup_preserves_queued_acquisitions_and_later_work_under_saturation() {
    use std::sync::mpsc;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("binary.dat"), [0, 255, 42]).unwrap();
    let (output, responses) = mpsc::sync_channel(32);
    let mut service = Service::new(root.path()).unwrap();
    service.histories = Some(history::Manager::new(output.clone()));
    let text_store = service.text.clone();
    let transfer = text_store
        .lock()
        .unwrap()
        .read(ropey::Rope::from_str("body"))
        .unwrap();
    let binary::Reply::Opened { source, .. } = service
        .binary
        .execute(
            root.path(),
            binary::Command::Open {
                path: path("binary.dat"),
            },
        )
        .unwrap()
    else {
        panic!()
    };
    // History is acquired by a queued job, not by fixture initialization. Its
    // close must follow that job even if cancellation beats the Start reply.
    let worker = actor("cleanup-order-test", service, output, Arc::default())
        .unwrap()
        .drain_on_disconnect();
    let held = text_store.lock().unwrap();
    let request = |id, command| Request {
        id,
        workspace: None,
        command,
    };
    worker
        .enqueue(
            request(
                1,
                Command::Text(text::Command::Read {
                    transfer,
                    start: 0,
                    end: 4,
                }),
            ),
            None,
        )
        .unwrap();
    worker
        .enqueue(
            request(
                2,
                Command::HistoryStart {
                    history: 42,
                    repository: path(""),
                },
            ),
            None,
        )
        .unwrap();
    for id in 3..=9 {
        let mut next = request(id, Command::Ping);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match worker.enqueue(next, None) {
                Ok(()) => break,
                Err(mpsc::TrySendError::Full(Some((_, (again, _)))))
                    if Instant::now() < deadline =>
                {
                    next = again;
                    std::thread::yield_now();
                }
                Err(error) => panic!("{error}"),
            }
        }
    }
    assert!(matches!(
        worker.enqueue(request(10, Command::Ping), None),
        Err(mpsc::TrySendError::Full(_))
    ));
    for _ in 0..16 {
        for resource in [
            ResourceClose::Text(transfer),
            ResourceClose::Binary(source),
            ResourceClose::History(42),
            ResourceClose::Rollback(42),
        ] {
            worker.close(resource).unwrap();
        }
    }
    // Same-valued IDs from different resource types cannot cancel one another.
    assert_eq!(worker.closes.entries.lock().unwrap().len(), 4);
    drop(held);
    for id in 1..=9 {
        let response = responses.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(response.id, id);
        assert!(response.result.is_ok(), "{response:?}");
    }
    assert!(text_store.lock().unwrap().snapshot(transfer).is_err());
    worker
        .enqueue(
            request(
                11,
                Command::Binary(binary::Command::Read {
                    source,
                    offset: 0,
                    count: 1,
                }),
            ),
            None,
        )
        .unwrap();
    worker
        .enqueue(
            request(
                12,
                Command::HistoryStart {
                    history: 42,
                    repository: path(""),
                },
            ),
            None,
        )
        .unwrap();
    worker
        .enqueue(request(13, Command::HistoryNext { history: 42 }), None)
        .unwrap();
    let stale = responses.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(stale.id, 11);
    assert!(
        stale.result.is_err(),
        "the released binary handle must be closed"
    );
    for id in 12..=13 {
        // The idle history only emits a page after Next; ignore its event.
        loop {
            let response = responses.recv_timeout(Duration::from_secs(3)).unwrap();
            if response.id == 0 {
                continue;
            }
            assert_eq!(response.id, id);
            assert!(response.result.is_ok(), "{response:?}");
            break;
        }
    }
    worker.close(ResourceClose::History(42)).unwrap();
    drop(worker);
}

#[test]
fn reply_channel_loss_releases_transfers_before_skipping_queued_work() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path()).unwrap();
    let store = service.text.clone();
    let transfer = store.lock().unwrap().read(ropey::Rope::from_str("body")).unwrap();
    let (output, replies) = std::sync::mpsc::sync_channel(1);
    let held = store.lock().unwrap();
    let worker = actor("cleanup-after-output-loss", service, output, Default::default()).unwrap().drain_on_disconnect();
    for id in 1..=3 {
        worker.enqueue(Request { id, workspace: None, command: Command::Text(text::Command::Read { transfer, start: 0, end: 4 }) }, None).unwrap();
    }
    worker.close(ResourceClose::Text(transfer)).unwrap();
    // The first response fails before requests 2/3 can run. Its cleanup barrier
    // still names ticket 3; teardown must release it without executing that work.
    drop(replies);
    drop(held);
    drop(worker);
    assert!(store.lock().unwrap().snapshot(transfer).is_err());
}

#[test]
fn resource_cleanup_is_bounded_coalesced_and_does_not_start_optional_workers() {
    use std::sync::mpsc;
    let root = tempfile::tempdir().unwrap();
    let (output, _responses) = mpsc::sync_channel(1);
    let lazy = LazyActor::new(
        "unused-cleanup",
        Service::new(root.path()).unwrap(),
        output,
        Arc::default(),
    );
    lazy.close(ResourceClose::History(1)).unwrap();
    assert!(lazy.actor.is_none());
    assert!(lazy.service.is_some());
    let (sender, requests) = mpsc::sync_channel(8);
    let worker = Actor {
        sender,
        queued: std::cell::Cell::new(0),
        closes: Arc::default(),
        worker: None,
        join: false,
    };
    for id in 0..2048 {
        worker.close(ResourceClose::Binary(id)).unwrap();
    }
    for _ in 0..16 {
        worker.close(ResourceClose::Binary(1)).unwrap();
    }
    assert_eq!(worker.closes.entries.lock().unwrap().len(), 2048);
    assert!(worker.close(ResourceClose::History(1)).is_err());
    assert_eq!(
        requests.try_iter().count(),
        1,
        "one pending cleanup batch needs only one wake"
    );
}

#[test]
fn closing_an_old_workspace_transfer_preserves_the_current_saved_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("second");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("file.txt"), "saved\n").unwrap();
    let first = tests::connection(root.path(), Arc::default(), Arc::default());
    let Reply::Text(text::Reply::Upload { transfer }) = first
        .call(Command::Text(text::Command::BeginBlob {
            characters: 0,
            bytes: 0,
        }))
        .unwrap()
    else {
        panic!()
    };
    let second = first.open_workspace(path("second")).unwrap();
    let Reply::Opened { revision, .. } = second
        .call(Command::Open {
            path: path("file.txt"),
        })
        .unwrap()
    else {
        panic!()
    };
    // A late transfer close from the first workspace is connection-wide. It
    // must not switch the file actor's root and throw away the second cache.
    std::fs::remove_file(nested.join("file.txt")).unwrap();
    first.close_text(transfer);
    first
        .notify(Command::Close {
            path: path("file.txt"),
        })
        .unwrap();
    let Reply::Text(text::Reply::Upload { transfer }) = second
        .call(Command::Text(text::Command::BeginSave {
            path: path("file.txt"),
            base: Some(revision),
            start: 0,
            end: 0,
            characters: 0,
            bytes: 0,
            encoding: "UTF-8".into(),
        }))
        .unwrap()
    else {
        panic!()
    };
    // Cached preparation is not permission to overwrite a vanished/changed
    // file. The durable save must still revalidate disk contents.
    assert!(
        second
            .call(Command::Text(text::Command::Finish { transfer }))
            .is_err()
    );
    assert!(!nested.join("file.txt").exists());
    first.call(Command::Shutdown).unwrap();
}

fn path(value: &str) -> RemotePath {
    RemotePath::new(value.as_bytes().to_vec()).unwrap()
}

#[test]
fn legacy_heartbeat_request_bypasses_a_blocked_file_worker() {
    use std::{io::{BufRead, Write}, net::{TcpListener, TcpStream}};
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path()).unwrap();
    let store = service.text.clone();
    let transfer = store.lock().unwrap().read(ropey::Rope::from_str("data")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let (server, _) = listener.accept().unwrap();
    let worker = std::thread::spawn(move || serve_service(service, server.try_clone().unwrap(), server));
    let mut reader = std::io::BufReader::new(client.try_clone().unwrap());
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line).unwrap();
    let hello = client::Client::parse_hello(&line).unwrap().unwrap();
    let held = store.lock().unwrap();
    let mut requests = Vec::new();
    for (id, command) in [
        (1, Command::Text(text::Command::Read { transfer, start: 0, end: 4 })),
        (2, Command::Heartbeat { nonce: 77 }),
    ] {
        wire::write_message(&mut requests, 'C', hello.session, id,
            &wire::encode(&Request { id, workspace: None, command }).unwrap()).unwrap();
    }
    client.write_all(&requests).unwrap();
    let result = (|| -> io::Result<Response> {
        let mut decoder = wire::Decoder::new('S', hello.session);
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 { return Err(io::ErrorKind::UnexpectedEof.into()); }
            if let Some((_, bytes)) = decoder.line(&line)? { return serde_json::from_slice(&bytes).map_err(io::Error::other); }
        }
    })();
    drop(held);
    let _ = client.shutdown(std::net::Shutdown::Both);
    let _ = worker.join().unwrap();
    let response = result.expect("A legacy request must not depend on the blocked file worker");
    assert_eq!(response.id, 2);
    assert!(matches!(response.result, Ok(Reply::Heartbeat { nonce: 77 })));
}

fn close_cached_blame_workload(
    workspace: workspace::Workspace,
    traffic: &AtomicUsize,
) -> (usize, usize, usize) {
    use crate::{
        app::{App, Tab},
        editor::Editor,
    };
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = true;
    app.terminal_has_focus = true;
    app.tabs.clear();
    for name in ["first.py", "second.py"] {
        let key = workspace.view_key(&path(name));
        let loaded = workspace.load(&key).unwrap();
        let mut editor = Editor::test_with_text("");
        editor.rope = loaded.rope;
        editor.saved_rope = Some(editor.rope.clone());
        editor.file_path = Some(key);
        editor.remote_checkpoint = loaded.remote_checkpoint;
        // This workload explicitly opens the blame rail to warm its cache.
        editor.blame.enabled = true;
        app.tabs.push(Tab::new_file(editor));
    }
    let finish = |app: &mut App| {
        app.wait_for_background(Duration::from_secs(20), |app| !app.bg_blame_running);
    };
    for index in 0..2 {
        app.active_tab = index;
        crate::git_blame::refresh(&mut app);
        finish(&mut app);
        assert!(
            crate::git_blame::cached(&app, app.editor().unwrap())
                .unwrap()
                .notice
                .is_none()
        );
    }
    let first = app.blame_documents[&app.tabs[0].editor.navigation_id].clone();
    // The user can spend minutes in the second file. Expiring the transport's
    // optional base cache must not invalidate the first editor's exact snapshot.
    workspace.git_source_state.lock().unwrap().checked = Instant::now() - Duration::from_secs(31);
    workspace.client.call(Command::Ping).unwrap();
    let before = traffic.load(Ordering::Relaxed);
    app.force_close_tab(1);
    crate::git_blame::refresh(&mut app);
    let scheduled = usize::from(app.bg_blame_running);
    finish(&mut app);
    let replaced = usize::from(!Arc::ptr_eq(
        &first,
        &app.blame_documents[&app.tabs[0].editor.navigation_id],
    ));
    let bytes = traffic.load(Ordering::Relaxed) - before;
    workspace.client.call(Command::Ping).unwrap();
    app.force_close_tab(0);
    crate::git_blame::refresh(&mut app);
    assert!(
        !app.bg_blame_running,
        "Closing the last file must not schedule blame"
    );
    workspace.client.call(Command::Ping).unwrap();
    (scheduled, replaced, bytes)
}

#[test]
fn remote_blame_browsing_and_typing_send_nothing_until_visible_idle_demand() {
    use crate::{
        app::{App, Tab},
        editor::Editor,
    };
    let root = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(root.path()).unwrap();
    let mut index = repo.index().unwrap();
    for name in ["first.py", "second.py"] {
        std::fs::write(root.path().join(name), "value = 42\n".repeat(100)).unwrap();
        index.add_path(Path::new(name)).unwrap();
    }
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        traffic.clone(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = true;
    app.terminal_has_focus = true;
    app.show_minimap = false;
    app.tabs.clear();
    for name in ["first.py", "second.py"] {
        let key = workspace.view_key(&path(name));
        let loaded = workspace.load(&key).unwrap();
        let mut editor = Editor::test_with_text("");
        editor.rope = loaded.rope;
        editor.saved_rope = Some(editor.rope.clone());
        editor.file_path = Some(key);
        editor.remote_checkpoint = loaded.remote_checkpoint;
        editor.word_wrap = false;
        app.tabs.push(Tab::new_file(editor));
    }
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    let before = traffic.load(Ordering::Relaxed);
    for index in (0..20).map(|index| index % 2) {
        app.active_tab = index;
        app.last_user_activity = Instant::now();
        app.editor_mut().unwrap().insert_char('x');
        terminal
            .draw(|frame| {
                crate::ui_inline_blame::begin_frame(&mut app);
                crate::ui_editor::draw_editor(frame, &mut app, frame.area());
            })
            .unwrap();
        crate::git_blame::refresh(&mut app);
        assert!(!app.bg_blame_running);
    }
    assert_eq!(
        traffic.load(Ordering::Relaxed),
        before,
        "Browsing and typing must not send blame requests, even with a real headless service attached"
    );
    assert!(app.blame_documents.is_empty());
    app.last_user_activity -= Duration::from_secs(2);
    crate::git_blame::refresh(&mut app);
    assert!(app.bg_blame_running);
    app.wait_for_background(Duration::from_secs(20), |app| !app.bg_blame_running);
    assert!(
        traffic.load(Ordering::Relaxed) > before,
        "Visible idle demand must still reach headless"
    );
    assert_eq!(
        app.blame_documents.len(),
        1,
        "Hidden tabs must not be prefetched"
    );
    let document = crate::git_blame::cached(&app, app.editor().unwrap()).unwrap();
    assert!(document.notice.is_none(), "{:?}", document.notice);
    assert!(matches!(
        document.lines[0],
        crate::git_blame::Line::Uncommitted
    ));
    let cached = traffic.load(Ordering::Relaxed);
    for _ in 0..20 {
        crate::git_blame::refresh(&mut app);
        assert!(!app.bg_blame_running);
        assert!(crate::git_blame::next_wait(&app, Instant::now()).is_none());
    }
    assert_eq!(
        traffic.load(Ordering::Relaxed),
        cached,
        "Idle cached blame must not poll headless"
    );
    workspace.client.call(Command::Shutdown).unwrap();
}

#[test]
fn closing_remote_tab_reuses_unchanged_blame_after_transport_cache_expiry() {
    let root = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(root.path()).unwrap();
    let mut index = repo.index().unwrap();
    for name in ["first.py", "second.py"] {
        std::fs::write(root.path().join(name), "value = 42\n".repeat(100)).unwrap();
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
        .unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        traffic.clone(),
    ));
    assert_eq!(
        close_cached_blame_workload(workspace, &traffic),
        (0, 0, 0),
        "Closing a tab must reuse the revealed file's exact cached annotations"
    );
}

#[test]
#[ignore = "Real SSH: VTCODE_CLOSE_SSH_ARGS must launch headless in a disposable Git workspace with committed first.py and second.py"]
fn ssh_closing_cached_files_does_not_restart_headless_analysis() {
    struct CountedInput(std::process::ChildStdin, Arc<AtomicUsize>);
    impl std::io::Write for CountedInput {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let written = self.0.write(bytes)?;
            self.1.fetch_add(written, Ordering::Relaxed);
            Ok(written)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
    let args = std::env::var_os("VTCODE_CLOSE_SSH_ARGS").expect("Disposable SSH argument JSON");
    let args: Vec<String> = serde_json::from_slice(&std::fs::read(args).unwrap()).unwrap();
    let child = std::process::Command::new(&args[0])
        .args(&args[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    // The SSH child is always reaped, including when an assertion unwinds.
    struct Stop(std::process::Child);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Stop(child);
    let traffic = Arc::new(AtomicUsize::new(0));
    let client = client::Client::connect(
        child.0.stdout.take().unwrap(),
        CountedInput(child.0.stdin.take().unwrap(), traffic.clone()),
    )
    .unwrap();
    let result = close_cached_blame_workload(workspace::Workspace::new(client.clone()), &traffic);
    client.call(Command::Shutdown).unwrap();
    assert!(child.0.wait().unwrap().success());
    println!(
        "VTCODE_SSH_CLOSE scheduled={} replaced={} close_request_bytes={}",
        result.0, result.1, result.2
    );
    assert_eq!(result, (0, 0, 0));
}

#[test]
fn location_open_classifies_files_and_directories_without_a_second_read() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("directory")).unwrap();
    std::fs::write(root.path().join("file.txt"), "hello\r\nλ\n").unwrap();
    let mut service = Service::new(root.path()).unwrap();
    let cancel = crate::background::CancellationToken::new();
    assert!(
        matches!(service.open_location(path("directory"), &cancel).unwrap(), Reply::Directory { identity } if identity == path("directory"))
    );
    let Reply::Opened {
        text,
        revision: saved,
        ..
    } = service.open_location(path("file.txt"), &cancel).unwrap()
    else {
        panic!()
    };
    assert_eq!(text, "hello\r\nλ\n");
    assert_eq!(saved, revision(&ropey::Rope::from_str(&text), "UTF-8"));
    std::fs::remove_file(root.path().join("file.txt")).unwrap();
    // The normal mutation worker received the same saved baseline, rather
    // than making a second Open solely to populate its document cache.
    assert!(
        service
            .documents
            .lock()
            .unwrap()
            .get(&service.root, &path("file.txt"))
            .is_some()
    );
    cancel.cancel();
    assert_eq!(
        service
            .open_location(path("directory"), &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn cancellable_content_revisions_match_saved_checkpoints_for_every_encoding() {
    let cancel = crate::background::CancellationToken::new();
    for text in ["", "\tλ 🦀\r\n", &"aλ\n".repeat(100_000)] {
        for encoding in ["UTF-8", "UTF-8 with BOM", "UTF-16 LE", "UTF-16 BE"] {
            let rope = ropey::Rope::from_str(text);
            assert_eq!(
                revision(&rope, encoding),
                revision_cancellable(&rope, encoding, &cancel).unwrap()
            );
        }
    }
    cancel.cancel();
    assert_eq!(
        revision_cancellable(&ropey::Rope::new(), "UTF-8", &cancel)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Interrupted
    );
}

#[cfg(unix)]
#[test]
fn native_and_headless_open_reject_named_pipes_without_waiting_for_a_writer() {
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("pipe.txt");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut observations = Vec::new();
    for mode in 0..4 {
        let cancel = crate::background::CancellationToken::new();
        let worker_cancel = cancel.clone();
        let worker_workspace = workspace.clone();
        let worker_path = fifo.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = match mode {
                0 => crate::editor::load_text_file(&worker_path).map(|_| ()),
                1 => worker_workspace
                    .open_location(
                        &worker_workspace.view_key(&path("pipe.txt")),
                        &worker_cancel,
                        |_, _| {},
                    )
                    .map(|_| ()),
                2 => crate::extension_documents::BinaryDocument::open_io(&worker_path).map(|_| ()),
                3 => binary::Source::open(
                    &worker_workspace,
                    &worker_workspace.view_key(&path("pipe.txt")),
                )
                .map(|_| ()),
                _ => unreachable!(),
            };
            let _ = send.send(result.map_err(|error| error.kind()));
        });
        let observed = receive.recv_timeout(Duration::from_secs(3));
        cancel.cancel();
        // Release a regressed blocking reader as well, so a failing test cannot
        // strand a service worker or hang cleanup.
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo),
        );
        worker.join().unwrap();
        observations.push((mode, observed));
        assert!(matches!(
            workspace.client.call(Command::Ping).unwrap(),
            Reply::Done
        ));
    }
    workspace.client.call(Command::Shutdown).unwrap();
    for (mode, observed) in observations {
        assert!(
            matches!(observed, Ok(Err(io::ErrorKind::InvalidInput))),
            "mode={mode}: {observed:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn copying_named_pipes_never_waits_for_a_writer_or_creates_an_empty_file() {
    use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut outcomes = Vec::new();
    for remote in [false, true] {
        let destination = root.path().join(if remote { "remote" } else { "native" });
        std::fs::create_dir(&destination).unwrap();
        let workspace = workspace::Workspace::new(tests::connection(
            root.path(),
            Arc::default(),
            Arc::default(),
        ));
        let source = fifo.clone();
        let target = destination.join("pipe");
        let work = target.clone();
        let worker_workspace = workspace.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = if remote {
                worker_workspace
                    .client
                    .call(Command::Filesystem(crate::remote::fs::Task::Paste {
                        sources: vec![path("pipe")],
                        directory: path("remote"),
                        cut: false,
                    }))
                    .map(|_| ())
            } else {
                crate::fs_ops::copy_no_replace(&source, &work, || false)
            };
            let _ = send.send(result);
        });
        let response = receive.recv_timeout(Duration::from_secs(3));
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&fifo),
        );
        worker.join().unwrap();
        workspace.client.call(Command::Shutdown).unwrap();
        let rejected =
            matches!(response, Ok(Err(ref error)) if error.kind() == io::ErrorKind::InvalidInput);
        outcomes.push((remote, rejected, target.exists()));
    }
    for (remote, rejected, created) in outcomes {
        assert!(
            rejected && !created,
            "remote={remote}, rejected={rejected}, created={created}"
        );
    }
}

#[test]
fn watched_file_index_survives_idle_and_invalidates_on_changes_or_watcher_failure() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("first.rs"), "first").unwrap();
    let mut service = Service::new(root.path()).unwrap();
    let observation = service.observations.get(&service.root);
    observation.active(true);
    let files = |service: &mut Service, known_version| {
        service
            .execute(Command::Files {
                generation: 1,
                known_version,
                excludes: Default::default(),
            })
            .unwrap()
    };
    let Reply::Files {
        version,
        files: Some(initial),
    } = files(&mut service, None)
    else {
        panic!()
    };
    assert_eq!(initial, [path("first.rs")]);
    let age = |service: &mut Service| {
        service.index.as_mut().unwrap().built = Instant::now() - Duration::from_secs(10);
    };
    age(&mut service);
    assert!(matches!(
        files(&mut service, Some(version)),
        Reply::Files { files: None, .. }
    ));
    assert!(
        service.index.as_ref().unwrap().built.elapsed() > Duration::from_secs(5),
        "An unchanged watched index must not be rebuilt after an idle timeout"
    );
    std::fs::write(root.path().join("second.rs"), "second").unwrap();
    observation.invalidate();
    let Reply::Files {
        version,
        files: Some(next),
    } = files(&mut service, Some(version))
    else {
        panic!()
    };
    assert_eq!(next.len(), 2);
    observation.active(false);
    std::fs::write(root.path().join("third.rs"), "third").unwrap();
    let Reply::Files {
        version,
        files: Some(next),
    } = files(&mut service, Some(version))
    else {
        panic!()
    };
    assert_eq!(next.len(), 3);
    age(&mut service);
    std::fs::write(root.path().join("fourth.rs"), "fourth").unwrap();
    assert!(
        matches!(files(&mut service, Some(version)), Reply::Files { files: Some(files), .. } if files.len() == 4),
        "Unwatched files retain the bounded polling fallback"
    );
}

#[test]
fn workspace_watch_observations_are_shared_without_cross_root_invalidations_or_retaining_old_roots()
{
    let registry = watch::Registry::default();
    let first = registry.get(Path::new("first"));
    let second = registry.get(Path::new("second"));
    assert!(Arc::ptr_eq(&first, &registry.get(Path::new("first"))));
    first.active(true);
    let stamp = second.stamp();
    first.invalidate();
    assert_eq!(stamp, second.stamp());
    let old = Arc::downgrade(&first);
    drop(first);
    assert!(old.upgrade().is_none());
    assert!(!registry.get(Path::new("first")).stamp().active);
}

#[test]
fn filename_index_rechecks_shared_filesystems_that_silently_omit_watch_events() {
    let root = tempfile::tempdir().unwrap();
    let mut service = Service::new(root.path()).unwrap();
    let observation = service.observations.get(&service.root);
    observation.active(true);
    let Reply::Files { version, .. } = service
        .execute(Command::Files {
            generation: 1,
            known_version: None,
            excludes: Default::default(),
        })
        .unwrap()
    else {
        panic!()
    };
    std::fs::write(root.path().join("unreported.rs"), "new").unwrap();
    service.index.as_mut().unwrap().built -= Duration::from_secs(31);
    assert!(
        matches!(service.execute(Command::Files { generation: 1, known_version: Some(version), excludes: Default::default() }).unwrap(), Reply::Files { files: Some(files), .. } if files == [path("unreported.rs")])
    );
}

#[test]
fn unused_headless_services_do_not_start_workers_and_first_use_reuses_its_worker() {
    let root = tempfile::tempdir().unwrap();
    let (sender, _output) = std::sync::mpsc::sync_channel(1);
    let mut service = LazyActor::new(
        "headless-lazy-test",
        Service::new(root.path()).unwrap(),
        sender,
        Default::default(),
    );
    assert!(service.actor.is_none());
    let worker = service
        .get()
        .unwrap()
        .worker
        .as_ref()
        .unwrap()
        .thread()
        .id();
    assert_eq!(
        service
            .get()
            .unwrap()
            .worker
            .as_ref()
            .unwrap()
            .thread()
            .id(),
        worker
    );
}

#[test]
fn saved_file_watcher_coverage_uses_canonical_identity_and_excludes_unwatched_paths() {
    let root = tempfile::tempdir().unwrap();
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut checkpoint = Checkpoint {
        revision: revision(&ropey::Rope::new(), "UTF-8"),
        conflicted: false,
        identity: Some(path("file.rs")),
    };
    let key = workspace.view_key(&path("file.rs"));
    assert!(workspace.save_is_watched(&key, Some(&checkpoint)));
    assert!(!workspace.save_is_watched(&key, None));
    checkpoint.identity = Some(path("elsewhere/target.rs"));
    assert!(
        !workspace.save_is_watched(&key, Some(&checkpoint)),
        "An alias must not claim coverage of its unobserved target"
    );
    let generated = path("target/generated.rs");
    checkpoint.identity = Some(generated.clone());
    assert!(workspace.save_is_watched(&workspace.view_key(&generated), Some(&checkpoint)));
    for value in [RemotePath::absolute(b"/outside/file.rs").unwrap()] {
        checkpoint.identity = Some(value.clone());
        assert!(!workspace.save_is_watched(&workspace.view_key(&value), Some(&checkpoint)));
    }
}

#[test]
fn opened_parent_repository_refresh_falls_back_to_full_scope_for_relative_worktree_events() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("first.txt"), "first").unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace =
        workspace::Workspace::new(tests::connection(&nested, Arc::default(), traffic.clone()));
    let cancel = crate::background::CancellationToken::new();
    let (initial, scans) = workspace
        .git_snapshot(true, None, true, Vec::new(), None, &cancel)
        .unwrap();
    assert!(initial.parent.is_some());
    assert_eq!(scans.len(), 1);
    let before = traffic.load(Ordering::Relaxed);
    std::fs::write(nested.join("second.txt"), "second").unwrap();
    let (_, next) = workspace
        .git_snapshot(
            true,
            Some(discovery(&workspace, &initial)),
            true,
            Vec::new(),
            Some(vec![path("second.txt")]),
            &cancel,
        )
        .unwrap();
    assert!(traffic.load(Ordering::Relaxed) > before);
    assert_eq!(next[0].files.len(), 1);
    assert!(next[0].files[0].directory);
}

#[cfg(windows)]
#[test]
fn windows_headless_absolute_drive_paths_roundtrip_unicode_and_save_without_reinterpreting_posix_paths()
 {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let native = outside.path().join("outside 文.txt");
    std::fs::write(&native, "original 🦀\r\n").unwrap();
    let native = native.canonicalize().unwrap();
    let root = root.path().canonicalize().unwrap();
    let path = git::from_native(&native, &root).unwrap();
    assert!(path.1);
    assert_eq!(path.resolve(&root).unwrap(), native);
    let mut service = Service::new(&root).unwrap();
    let Reply::Opened { revision, text, .. } = service
        .execute(Command::Open { path: path.clone() })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(text, "original 🦀\r\n");
    service
        .execute(Command::Save {
            path,
            base: Some(revision),
            encoding: "UTF-8".into(),
            change: Change::between(
                &ropey::Rope::from_str(&text),
                &ropey::Rope::from_str("saved 文\r\n"),
            ),
        })
        .unwrap();
    assert_eq!(std::fs::read_to_string(native).unwrap(), "saved 文\r\n");
    for invalid in [b"/tmp/file".as_slice(), b"/C:relative", b"/C:/file:stream"] {
        assert!(
            RemotePath::absolute(invalid)
                .unwrap()
                .resolve(&root)
                .is_err()
        );
    }
}

fn discovery(
    workspace: &workspace::Workspace,
    native: &crate::git_repositories::Discovery,
) -> git::Discovery {
    git::Discovery {
        errors: Vec::new(),
        roots: native
            .roots
            .iter()
            .map(|root| workspace.remote_path(root).unwrap())
            .collect(),
        parent: native
            .parent
            .as_ref()
            .map(|root| workspace.remote_path(root).unwrap()),
    }
}

#[test]
fn worktree_refresh_skips_unrelated_repositories_and_keeps_committed_sources_until_git_changes() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let first_repo = git2::Repository::init(&first).unwrap();
    git2::Repository::init(&second).unwrap();
    std::fs::write(first.join("one.rs"), "one\n").unwrap();
    std::fs::write(second.join("two.rs"), "two\n").unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        traffic.clone(),
    ));
    let cancel = crate::background::CancellationToken::new();
    let (initial, scans) = workspace
        .git_snapshot(true, None, false, Vec::new(), None, &cancel)
        .unwrap();
    assert_eq!(scans.len(), 2);
    let known = discovery(&workspace, &initial);
    let key = workspace.view_key(&path("first/one.rs"));
    let source = workspace.git_sources(&key, None, false).unwrap();
    let before = traffic.load(Ordering::Relaxed);
    let (_, unchanged) = workspace
        .git_snapshot(
            true,
            Some(known.clone()),
            false,
            Vec::new(),
            Some(vec![path("notes.txt")]),
            &cancel,
        )
        .unwrap();
    assert_eq!(unchanged.len(), 2);
    assert_eq!(
        traffic.load(Ordering::Relaxed),
        before,
        "Saving outside every repository must cause zero Git protocol traffic"
    );
    assert!(Arc::ptr_eq(
        &source,
        &workspace.git_sources(&key, None, false).unwrap()
    ));

    std::fs::write(first.join("new.rs"), "new").unwrap();
    let (_, changed) = workspace
        .git_snapshot(
            true,
            Some(known.clone()),
            false,
            Vec::new(),
            Some(vec![path("first/new.rs")]),
            &cancel,
        )
        .unwrap();
    assert_eq!(
        changed
            .iter()
            .find(|scan| scan.root == workspace.view_key(&path("first")))
            .unwrap()
            .files
            .len(),
        2
    );
    assert_eq!(
        changed
            .iter()
            .find(|scan| scan.root == workspace.view_key(&path("second")))
            .unwrap()
            .files
            .len(),
        1
    );
    assert!(
        Arc::ptr_eq(&source, &workspace.git_sources(&key, None, false).unwrap()),
        "Worktree edits do not change the index or committed blob"
    );

    let mut index = first_repo.index().unwrap();
    index.add_path(Path::new("one.rs")).unwrap();
    index.write().unwrap();
    workspace
        .git_snapshot(true, Some(known.clone()), false, Vec::new(), None, &cancel)
        .unwrap();
    let staged = workspace.git_sources(&key, None, false).unwrap();
    assert!(!Arc::ptr_eq(&source, &staged));
    assert_eq!(staged.index.as_deref(), Some("one\n"));

    // Committed sources have their own age bound: frequent worktree/status
    // refreshes must not indefinitely hide a silently changed index on NFS.
    index.remove_path(Path::new("one.rs")).unwrap();
    index.write().unwrap();
    workspace.git_source_state.lock().unwrap().checked -= Duration::from_secs(31);
    let rechecked_source = workspace.git_sources(&key, None, false).unwrap();
    assert!(!Arc::ptr_eq(&staged, &rechecked_source));
    assert!(rechecked_source.index.is_none());

    // NFS and other shared disks may omit notifications. Expiry on the next
    // query still discovers an unrelated edit without adding idle polling.
    let mut cached = workspace
        .git_snapshots
        .lock()
        .unwrap()
        .get(&path("second"))
        .unwrap()
        .clone();
    cached.checked -= Duration::from_secs(31);
    workspace
        .git_snapshots
        .lock()
        .unwrap()
        .insert(path("second"), 1024, cached);
    std::fs::write(second.join("unreported.rs"), "network filesystem edit").unwrap();
    let before = traffic.load(Ordering::Relaxed);
    let (_, rechecked) = workspace
        .git_snapshot(
            true,
            Some(known.clone()),
            false,
            Vec::new(),
            Some(vec![path("notes.txt")]),
            &cancel,
        )
        .unwrap();
    assert!(traffic.load(Ordering::Relaxed) > before);
    assert_eq!(
        rechecked
            .iter()
            .find(|scan| scan.root == workspace.view_key(&path("second")))
            .unwrap()
            .files
            .len(),
        2
    );

    // A closed repository disappears from the result; reopening explicitly
    // refreshes it. A missing/evicted snapshot also takes the normal scan path.
    let (_, open) = workspace
        .git_snapshot(
            true,
            Some(known.clone()),
            false,
            vec![path("second")],
            Some(vec![path("notes.txt")]),
            &cancel,
        )
        .unwrap();
    assert_eq!(open.len(), 1);
    workspace
        .git_snapshots
        .lock()
        .unwrap()
        .remove(&path("second"));
    let before = traffic.load(Ordering::Relaxed);
    let (_, open) = workspace
        .git_snapshot(
            true,
            Some(known),
            false,
            Vec::new(),
            Some(vec![path("notes.txt")]),
            &cancel,
        )
        .unwrap();
    assert_eq!(open.len(), 2);
    assert!(traffic.load(Ordering::Relaxed) > before);
    cancel.cancel();
    assert!(
        workspace
            .git_snapshot(true, None, false, Vec::new(), None, &cancel)
            .is_err()
    );
}

struct PausedReply {
    hold: Arc<AtomicBool>,
    blocked: Arc<AtomicBool>,
}

impl PausedReply {
    fn wait(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.blocked.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            self.blocked.load(Ordering::Acquire),
            "Reply was not prepared"
        );
    }
    fn release(&self) {
        self.hold.store(false, Ordering::Release);
    }
}

impl Drop for PausedReply {
    fn drop(&mut self) {
        self.release();
    }
}

fn paused_connection(root: &Path) -> (workspace::Workspace, PausedReply, Arc<AtomicUsize>) {
    let pause = PausedReply {
        hold: Arc::default(),
        blocked: Arc::default(),
    };
    let traffic = Arc::new(AtomicUsize::new(0));
    let client = tests::connection_with_status(
        root,
        pause.hold.clone(),
        traffic.clone(),
        pause.blocked.clone(),
    );
    pause.hold.store(true, Ordering::Release);
    (workspace::Workspace::new(client), pause, traffic)
}

#[test]
fn delayed_git_reply_cannot_restore_sources_invalidated_by_index_changes() {
    let root = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(root.path()).unwrap();
    std::fs::write(root.path().join("file.rs"), "before\n").unwrap();
    let (workspace, pause, traffic) = paused_connection(root.path());
    let key = workspace.view_key(&path("file.rs"));
    let pending = {
        let workspace = workspace.clone();
        let key = key.clone();
        std::thread::spawn(move || workspace.git_sources(&key, None, false).unwrap())
    };
    pause.wait();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file.rs")).unwrap();
    index.write().unwrap();
    workspace.refresh_git_source_cache(true);
    pause.release();
    let previous = pending.join().unwrap();
    assert!(previous.index.is_none());
    let before = traffic.load(Ordering::Relaxed);
    let current = workspace.git_sources(&key, None, false).unwrap();
    assert_eq!(current.index.as_deref(), Some("before\n"));
    assert!(!Arc::ptr_eq(&previous, &current));
    assert!(traffic.load(Ordering::Relaxed) > before);
}

#[test]
fn delayed_blame_reply_cannot_restore_sources_invalidated_by_head_changes() {
    let root = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(root.path()).unwrap();
    let commit = |text: &str| {
        std::fs::write(root.path().join("file.rs"), text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("file.rs")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.invalid").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            text,
            &tree,
            &parent.iter().collect::<Vec<_>>(),
        )
        .unwrap()
        .to_string()
    };
    let previous_hash = commit("before\n");
    let (workspace, pause, traffic) = paused_connection(root.path());
    let key = workspace.view_key(&path("file.rs"));
    let prepare = |workspace: &workspace::Workspace, key: PathBuf, text: &str| {
        workspace.prepare_blame(
            key,
            ropey::Rope::from_str(text),
            None,
            &crate::background::CancellationToken::new(),
        )
    };
    let pending = {
        let workspace = workspace.clone();
        let key = key.clone();
        std::thread::spawn(move || prepare(&workspace, key, "before\n"))
    };
    pause.wait();
    let current_hash = commit("after\n");
    workspace.refresh_git_source_cache(true);
    pause.release();
    let previous = pending.join().unwrap();
    assert!(
        matches!(&previous.lines[0], crate::git_blame::Line::Committed(commit) if commit.hash == previous_hash)
    );
    let before = traffic.load(Ordering::Relaxed);
    let current = prepare(&workspace, key, "after\n");
    assert!(
        matches!(&current.lines[0], crate::git_blame::Line::Committed(commit) if commit.hash == current_hash)
    );
    assert!(traffic.load(Ordering::Relaxed) > before);
}

#[test]
fn pending_worktree_refreshes_merge_and_a_full_refresh_cannot_be_downgraded() {
    let root = tempfile::tempdir().unwrap();
    let workspace = workspace::Workspace::new(tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut app = crate::app::App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = true;
    app.workspace_watcher_active = true;
    app.bg_git_status_running = true;
    app.refresh_source_control_changed_paths([workspace.view_key(&path("first.rs"))]);
    app.refresh_source_control_changed_paths([workspace.view_key(&path("second.rs"))]);
    assert_eq!(app.pending_git_status_paths.as_ref().unwrap().len(), 2);
    app.refresh_source_control_async();
    assert!(app.pending_git_status_paths.is_none());
    app.refresh_source_control_changed_paths([workspace.view_key(&path("third.rs"))]);
    assert!(
        app.pending_git_status_paths.is_none(),
        "An explicit/metadata refresh must retain its full scope"
    );
    app.source_control_enabled = false;
}

#[test]
fn delayed_metadata_retains_request_time_for_save_and_checkpoint_barriers() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.rs"), "before\n").unwrap();
    let (workspace, pause, _) = paused_connection(root.path());
    let pending = {
        let workspace = workspace.clone();
        std::thread::spawn(move || {
            workspace
                .inspect(
                    &[workspace.view_key(&path("file.rs"))],
                    &crate::background::CancellationToken::new(),
                )
                .unwrap()
        })
    };
    pause.wait();
    let barrier = Instant::now();
    std::fs::write(root.path().join("file.rs"), "after\n").unwrap();
    pause.release();
    let changes = pending.join().unwrap();
    assert_eq!(changes[0].len, Some(7));
    assert!(
        changes[0].observed_at < barrier,
        "A delayed old observation must not look newer than a save/checkpoint"
    );
}

#[test]
fn headless_git_status_sends_only_literal_worktree_deltas_and_full_rule_updates() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    for name in ["literal[1].txt", "literal1.txt"] { std::fs::write(root.path().join(name), "original\n").unwrap(); }
    let mut service = Service::new(root.path()).unwrap();
    let Reply::GitStatus { discovery: known, scans } = service.execute(Command::GitStatus {
        known: None, parent_open: false, closed: Vec::new(), include_untracked: true,
        changed: Some(vec![path("literal[1].txt")]),
    }).unwrap() else { panic!(); };
    assert_eq!(scans[0].files.len(), 2, "Discovery must return a complete initial snapshot");
    assert!(scans[0].worktree_paths.is_none());
    let mut query = |changed: Vec<RemotePath>| match service.execute(Command::GitStatus {
        known: Some(known.clone()), parent_open: false, closed: Vec::new(), include_untracked: true,
        changed: Some(changed),
    }).unwrap() { Reply::GitStatus { scans, .. } => scans, _ => panic!() };
    let scans = query(vec![path("literal[1].txt")]);
    assert_eq!(scans[0].files.len(), 1);
    assert_eq!(scans[0].files[0].path, path("literal[1].txt"));
    assert_eq!(scans[0].worktree_paths.as_ref().unwrap().paths, [path("literal[1].txt")]);
    assert!(query(Vec::new()).is_empty());
    let scans = query(vec![path(".git/index")]);
    assert!(scans[0].worktree_paths.is_none());
    assert_eq!(scans[0].files.len(), 2);
    std::fs::write(root.path().join(".gitignore"), "literal1.txt\n").unwrap();
    let scans = query(vec![path(".gitignore")]);
    assert!(scans[0].worktree_paths.is_none());
    assert!(!scans[0].files.iter().any(|file| file.path == path("literal1.txt")));
}

#[test]
fn remote_status_deltas_do_not_extend_full_snapshot_expiry() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    std::fs::write(root.path().join("reported.txt"), "first\n").unwrap();
    let workspace = workspace::Workspace::new(tests::connection(root.path(), Arc::default(), Arc::default()));
    let cancel = crate::background::CancellationToken::new();
    let (initial, _) = workspace.git_snapshot(true, None, false, Vec::new(), None, &cancel).unwrap();
    let known = discovery(&workspace, &initial);
    let checked = workspace.git_snapshots.lock().unwrap().get(&path("")).unwrap().checked;
    std::fs::write(root.path().join("unreported.txt"), "missed event\n").unwrap();
    for _ in 0..3 {
        let (_, scans) = workspace.git_snapshot(true, Some(known.clone()), false, Vec::new(),
            Some(vec![path("reported.txt")]), &cancel).unwrap();
        assert_eq!(scans[0].files.len(), 1, "Unrelated paths must not be rescanned by an ordinary delta");
        assert_eq!(workspace.git_snapshots.lock().unwrap().get(&path("")).unwrap().checked, checked);
    }
    let mut cached = workspace.git_snapshots.lock().unwrap().take(&path("")).unwrap();
    cached.checked -= Duration::from_secs(31);
    workspace.git_snapshots.lock().unwrap().insert(path(""), 1024, cached);
    let (_, scans) = workspace.git_snapshot(true, Some(known.clone()), false, Vec::new(),
        Some(vec![path("reported.txt")]), &cancel).unwrap();
    assert_eq!(scans[0].files.len(), 2, "Expiry must discover a missed change outside the delta");
    workspace.git_snapshots.lock().unwrap().remove(&path(""));
    std::fs::write(root.path().join("uncached.txt"), "after eviction\n").unwrap();
    let (_, scans) = workspace.git_snapshot(true, Some(known), false, Vec::new(),
        Some(vec![path("reported.txt")]), &cancel).unwrap();
    assert_eq!(scans[0].files.len(), 3, "An evicted baseline requires a full snapshot");
}

#[test]
fn remote_navigation_reuses_status_but_expiry_explicit_refresh_and_unwatched_paths_revalidate() {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    std::fs::write(root.path().join("first.txt"), "first\n").unwrap();
    let workspace = workspace::Workspace::new(tests::connection(root.path(), Arc::default(), Arc::default()));
    let mut app = crate::app::App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.workspace_watcher_active = true;
    app.refresh_source_control_async();
    let settle = |app: &mut crate::app::App| app.wait_for_background(Duration::from_secs(15), |app|
        !app.bg_git_log_running && !app.bg_git_branch_running && !app.bg_git_branch_pending
            && !app.bg_git_status_running && !app.bg_git_status_pending);
    settle(&mut app);
    let before = workspace.client.next_request_id_for_test();
    app.source_control_last_refresh = Some(Instant::now() - Duration::from_secs(3));
    app.refresh_source_control_for_navigation();
    settle(&mut app);
    assert_eq!(workspace.client.next_request_id_for_test(), before, "Navigation must not generate Git traffic for fresh snapshots");
    let mut cached = workspace.git_snapshots.lock().unwrap().take(&path("")).unwrap();
    cached.checked -= Duration::from_secs(31);
    workspace.git_snapshots.lock().unwrap().insert(path(""), 1024, cached);
    std::fs::write(root.path().join("missed.txt"), "missed filesystem event\n").unwrap();
    app.source_control_last_refresh = Some(Instant::now() - Duration::from_secs(3));
    app.refresh_source_control_for_navigation();
    settle(&mut app);
    assert!(workspace.client.next_request_id_for_test() > before);
    assert!(app.source_control_files.iter().any(|file| file.path == "missed.txt"));
    let before = workspace.client.next_request_id_for_test();
    app.refresh_source_control_async();
    settle(&mut app);
    assert!(workspace.client.next_request_id_for_test() > before, "Explicit refresh must bypass cached status");
    let before = workspace.client.next_request_id_for_test();
    app.workspace_watcher_active = false;
    app.source_control_last_refresh = Some(Instant::now() - Duration::from_secs(3));
    app.refresh_source_control_for_navigation();
    settle(&mut app);
    assert!(workspace.client.next_request_id_for_test() > before, "Unavailable watches retain full refreshes");
    let before = workspace.client.next_request_id_for_test();
    app.workspace_watcher_active = true;
    app.source_control_repository_state = crate::app::SourceControlRepositoryState::Missing;
    app.source_control_last_refresh = Some(Instant::now() - Duration::from_secs(3));
    app.refresh_source_control_for_navigation();
    settle(&mut app);
    assert!(workspace.client.next_request_id_for_test() > before, "Missing repositories must not be negatively cached by navigation");
}

#[cfg(unix)]
#[test]
fn headless_git_status_keeps_non_unicode_paths_lossless_when_a_delta_falls_back() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    for bytes in [b"name-\xfe.txt", b"name-\xff.txt"] {
        std::fs::write(root.path().join(std::ffi::OsString::from_vec(bytes.to_vec())), "source\n").unwrap();
    }
    let mut service = Service::new(root.path()).unwrap();
    let Reply::GitStatus { discovery: known, .. } = service.execute(Command::GitStatus {
        known: None, parent_open: false, closed: Vec::new(), include_untracked: true, changed: None,
    }).unwrap() else { panic!(); };
    let Reply::GitStatus { scans, .. } = service.execute(Command::GitStatus {
        known: Some(known), parent_open: false, closed: Vec::new(), include_untracked: true,
        changed: Some(vec![RemotePath::new(b"name-\xfe.txt".to_vec()).unwrap()]),
    }).unwrap() else { panic!(); };
    assert!(scans[0].worktree_paths.is_none());
    assert_eq!(scans[0].files.len(), 2);
    assert_ne!(scans[0].files[0].path, scans[0].files[1].path);
}
