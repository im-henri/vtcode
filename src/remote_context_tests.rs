use super::*;

fn execute(root: &Path, command: Command, cancel: &CancellationToken) -> io::Result<Reply> {
    super::execute(root, command, cancel, &Default::default())
}

#[test]
fn context_reads_exact_utf8_boundaries_and_rejects_binary_or_oversized_files() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("context.txt");
    let cancel = CancellationToken::new();
    let text = "λ".repeat(FILE_LIMIT / 2);
    std::fs::write(&path, &text).unwrap();
    assert_eq!(read_native(&path, &cancel).unwrap(), text);
    for bad in [vec![0, b'a'], vec![255], vec![b'a'; FILE_LIMIT + 1]] {
        std::fs::write(&path, bad).unwrap();
        assert!(read_native(&path, &cancel).is_err());
    }
    std::fs::File::create(&path)
        .unwrap()
        .set_len(70 * 1024 * 1024)
        .unwrap();
    assert!(
        read_native(&path, &cancel)
            .unwrap_err()
            .to_string()
            .contains("128 KiB")
    );
    assert!(read_native(root.path(), &cancel).is_err());
    cancel.cancel();
    assert_eq!(
        read_native(&path, &cancel).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn context_inspection_preserves_new_files_and_enforces_batch_limits() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (path, kind) = inspect_native(&root.join("new/file.rs")).unwrap();
    assert_eq!(path, root.join("new/file.rs"));
    assert_eq!(kind, Kind::Missing);
    let cancel = CancellationToken::new();
    assert!(
        execute(
            &root,
            Command::Inspect {
                paths: vec![RemotePath::default(); INSPECT_BATCH + 1]
            },
            &cancel
        )
        .is_err()
    );
    assert!(
        execute(
            &root,
            Command::Files {
                files: vec![(RemotePath::default(), RemotePath::default()); FILE_COUNT + 1],
                retain: false,
            },
            &cancel
        )
        .is_err()
    );
    cancel.cancel();
    assert_eq!(
        execute(
            &root,
            Command::Inspect {
                paths: vec![RemotePath::default()]
            },
            &cancel
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn context_batch_enforces_total_text_budget() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("context.txt");
    std::fs::write(&native, "a".repeat(FILE_LIMIT)).unwrap();
    let path = RemotePath::new(b"context.txt".to_vec()).unwrap();
    let root = root.path().canonicalize().unwrap();
    let cancel = CancellationToken::new();
    let Reply::Files { sizes, source } = execute(
        &root,
        Command::Files {
            files: vec![(path.clone(), path.clone()); 8],
            retain: false,
        },
        &cancel,
    )
    .unwrap() else {
        panic!("text response");
    };
    assert_eq!(sizes.iter().sum::<usize>(), TOTAL_LIMIT);
    assert!(source.is_none());
    assert!(
        execute(
            &root,
            Command::Files {
                files: vec![(path.clone(), path); 9],
                retain: false,
            },
            &cancel
        )
        .unwrap_err()
        .to_string()
        .contains("1 MiB")
    );
}

fn context_path(value: &str) -> RemotePath {
    RemotePath::new(value.as_bytes().to_vec()).unwrap()
}

#[test]
fn frozen_context_transfers_metadata_only_and_keeps_disk_text_through_changes() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use vtcode::acp::{self, protocol as p};
    let root = tempfile::tempdir().unwrap();
    let text = "frozen λ\r\n\"quoted\"\n".repeat(4000);
    std::fs::write(root.path().join("context.txt"), &text).unwrap();
    let received = Arc::new(AtomicUsize::new(0));
    let workspace = workspace::Workspace::new(super::super::tests::connection_with_traffic(
        root.path(),
        Arc::default(),
        Arc::default(),
        Arc::default(),
        received.clone(),
    ));
    let key = workspace.view_key(&context_path("context.txt"));
    let before = received.load(Ordering::Relaxed);
    let (sizes, source) = workspace
        .file_context(
            &[(key.clone(), key.clone())],
            true,
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(sizes, [text.len()]);
    let response_bytes = received.load(Ordering::Relaxed) - before;
    eprintln!(
        "VTCODE_CONTEXT_TRAFFIC {}",
        serde_json::json!({"text_bytes":text.len(), "prepare_response_bytes":response_bytes})
    );
    assert!(response_bytes < 2048, "File bodies must stay on the node");
    let before = received.load(Ordering::Relaxed);
    let (validated, temporary) = workspace
        .file_context(&[(key.clone(), key)], false, &CancellationToken::new())
        .unwrap();
    assert_eq!(validated, sizes);
    assert!(temporary.is_none());
    assert!(
        received.load(Ordering::Relaxed) - before < 2048,
        "Attachment validation must not download the body either"
    );
    let source = source.unwrap();
    let command = acp::Command::Prompt {
        session_id: p::SessionId::new("test"),
        content: vec![source.reference(0, true, "file".into())],
    };
    let retained = retain(&workspace.client, &command).unwrap();
    let weak = Arc::downgrade(&source);
    drop(source);
    assert!(weak.upgrade().is_some());
    drop(retained);
    assert!(weak.upgrade().is_none());
    assert!(retain(&workspace.client, &command).is_err());

    // Test materialization with the actual node store, independently of disk
    // contents or paths disappearing after the immutable snapshot is prepared.
    let root = root.path().canonicalize().unwrap();
    let store = std::sync::Mutex::new(text::Store::default());
    let Reply::Files {
        source: Some(id), ..
    } = super::execute(
        &root,
        Command::Files {
            files: vec![(context_path("context.txt"), context_path("context.txt"))],
            retain: true,
        },
        &CancellationToken::new(),
        &store,
    )
    .unwrap()
    else {
        panic!("snapshot")
    };
    std::fs::remove_file(root.join("context.txt")).unwrap();
    for embedded in [false, true] {
        let mut command = acp::Command::Prompt {
            session_id: p::SessionId::new("test"),
            content: vec![p::ContentBlock::ResourceLink(p::ResourceLink::new(
                "context",
                format!("vtcode-context:{id}:0:{}", u8::from(embedded)),
            ))],
        };
        materialize(&root, &store, &mut command).unwrap();
        let acp::Command::Prompt { content, .. } = command else {
            unreachable!()
        };
        if embedded {
            let p::ContentBlock::Resource(resource) = &content[0] else {
                panic!("embedded")
            };
            let p::EmbeddedResourceResource::TextResourceContents(resource) = &resource.resource
            else {
                panic!("text")
            };
            assert_eq!(resource.text, text);
            assert_eq!(
                url::Url::parse(&resource.uri).unwrap(),
                url::Url::from_file_path(root.join("context.txt")).unwrap()
            );
        } else {
            let p::ContentBlock::Text(block) = &content[0] else {
                panic!("fallback")
            };
            let data: serde_json::Value =
                serde_json::from_str(block.text.split_once('\n').unwrap().1).unwrap();
            assert_eq!(data["text"], text);
        }
    }
}

#[test]
fn context_snapshots_do_not_consume_file_transfer_slots_and_reject_stale_or_foreign_references() {
    use vtcode::acp::{self, protocol as p};
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let store = std::sync::Mutex::new(text::Store::default());
    let snapshot = || Snapshot {
        root: root.clone(),
        files: vec![(context_path("file"), "frozen".into())],
    };
    let mut ids = Vec::new();
    for _ in 0..32 {
        ids.push(store.lock().unwrap().context(snapshot()).unwrap());
    }
    assert_eq!(
        store
            .lock()
            .unwrap()
            .context(snapshot())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let mut files = Vec::new();
    for _ in 0..8 {
        files.push(
            store
                .lock()
                .unwrap()
                .read(ropey::Rope::from_str("open text"))
                .unwrap(),
        );
    }
    assert_eq!(
        store
            .lock()
            .unwrap()
            .read(ropey::Rope::new())
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let command = |id, index| acp::Command::Prompt {
        session_id: p::SessionId::new("test"),
        content: vec![p::ContentBlock::ResourceLink(p::ResourceLink::new(
            "context",
            format!("vtcode-context:{id}:{index}:0"),
        ))],
    };
    assert!(materialize(&root.join("other"), &store, &mut command(ids[0], 0)).is_err());
    assert!(materialize(&root, &store, &mut command(ids[0], 1)).is_err());
    store.lock().unwrap().close(ids[0]);
    assert!(materialize(&root, &store, &mut command(ids[0], 0)).is_err());
    store.lock().unwrap().context(snapshot()).unwrap();
    assert_eq!(store.lock().unwrap().chunk(files[0], 0..4).unwrap(), "open");
}

#[test]
fn cancelled_context_delivery_and_dropped_sources_release_all_snapshot_slots() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::{Duration, Instant};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file"), "context").unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let _release = Release(hold.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let workspace = workspace::Workspace::new(super::super::tests::connection_with_status(
        root.path(),
        hold.clone(),
        Arc::default(),
        blocked.clone(),
    ));
    let key = workspace.view_key(&context_path("file"));
    let cancel = CancellationToken::new();
    hold.store(true, Ordering::Release);
    let pending = {
        let workspace = workspace.clone();
        let key = key.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || workspace.file_context(&[(key.clone(), key)], true, &cancel))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !blocked.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(blocked.load(Ordering::Acquire));
    cancel.cancel();
    assert_eq!(
        pending.join().unwrap().unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    hold.store(false, Ordering::Release);
    workspace.client.call(super::super::Command::Ping).unwrap();
    let mut sources = Vec::new();
    for _ in 0..32 {
        sources.push(
            workspace
                .file_context(
                    &[(key.clone(), key.clone())],
                    true,
                    &CancellationToken::new(),
                )
                .unwrap()
                .1
                .unwrap(),
        );
    }
    drop(sources);
}

#[cfg(unix)]
#[test]
fn context_read_rejects_retargeted_alias_and_never_waits_for_a_fifo_writer() {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("one"), "one").unwrap();
    std::fs::write(root.path().join("two"), "two").unwrap();
    let alias = root.path().join("alias");
    std::os::unix::fs::symlink("one", &alias).unwrap();
    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink("two", &alias).unwrap();
    let file = RemotePath::new(b"alias".to_vec()).unwrap();
    let expected = RemotePath::new(b"one".to_vec()).unwrap();
    let cancel = CancellationToken::new();
    assert!(
        execute(
            root.path(),
            Command::Files {
                files: vec![(file, expected)],
                retain: false,
            },
            &cancel
        )
        .unwrap_err()
        .to_string()
        .contains("link changed")
    );
    let fifo = root.path().join("pipe");
    let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert!(read_native(&fifo, &cancel).is_err());
}
