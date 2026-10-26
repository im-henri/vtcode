use super::*;
use crate::workspace_replace::{File, Options};
use std::sync::atomic::{AtomicUsize, Ordering};

fn path(name: &str) -> RemotePath {
    RemotePath::new(name.as_bytes().to_vec()).unwrap()
}
fn options() -> Options {
    Options {
        query: "(foo)".into(),
        replacement: "$1猫".into(),
        case_sensitive: true,
        regex: true,
        whole_word: false,
        preserve_case: false,
    }
}

#[test]
fn workspace_replace_node_batches_closed_files_without_transfer_and_invalidates_only_written_checkpoints()
 {
    let root = tempfile::tempdir().unwrap();
    let line = "unchanged context λ\n";
    let source = format!(
        "foo\r\n{}",
        line.repeat(super::super::text::INLINE_BYTES / line.len() + 1024)
    );
    for name in ["first.txt", "second.txt", "untouched.txt"] {
        std::fs::write(root.path().join(name), &source).unwrap();
    }
    let outgoing = Arc::new(AtomicUsize::new(0));
    let incoming = Arc::new(AtomicUsize::new(0));
    let client = super::super::tests::connection_with_traffic(
        root.path(),
        Arc::default(),
        outgoing.clone(),
        Arc::default(),
        incoming.clone(),
    );
    let Reply::Text(super::super::text::Reply::Opened {
        transfer,
        revision: old,
        ..
    }) = client
        .call(Command::Open {
            path: path("first.txt"),
        })
        .unwrap()
    else {
        panic!()
    };
    client.close_text(transfer);
    let Reply::Text(super::super::text::Reply::Opened {
        transfer,
        revision: untouched_revision,
        ..
    }) = client
        .call(Command::Open {
            path: path("untouched.txt"),
        })
        .unwrap()
    else {
        panic!()
    };
    client.close_text(transfer);
    let before = (
        outgoing.load(Ordering::Relaxed),
        incoming.load(Ordering::Relaxed),
    );
    let Reply::Replaced(outcome) = client
        .call(Command::Replace(Replace {
            options: options(),
            files: vec![
                File {
                    path: path("first.txt"),
                    matches: vec![(1, 0, 3)],
                },
                File {
                    path: path("second.txt"),
                    matches: vec![(1, 0, 3)],
                },
            ],
        }))
        .unwrap()
    else {
        panic!()
    };
    let transferred = (
        outgoing.load(Ordering::Relaxed) - before.0,
        incoming.load(Ordering::Relaxed) - before.1,
    );
    assert_eq!(
        (
            outcome.replacements,
            outcome.files_changed,
            outcome.written_files,
            outcome.failures
        ),
        (2, 2, 2, 0)
    );
    assert!(
        transferred.0 < 8192 && transferred.1 < 8192,
        "closed file bodies must stay on node: {transferred:?}"
    );
    let expected = source.replacen("foo", "foo猫", 1);
    for name in ["first.txt", "second.txt"] {
        assert_eq!(
            std::fs::read_to_string(root.path().join(name)).unwrap(),
            expected
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join("untouched.txt")).unwrap(),
        source
    );
    let stale = client.call(Command::Text(super::super::text::Command::BeginSave {
        path: path("first.txt"),
        base: Some(old),
        start: 0,
        end: 0,
        characters: 0,
        bytes: 0,
        encoding: "UTF-8".into(),
    }));
    if let Ok(Reply::Text(super::super::text::Reply::Upload { transfer })) = &stale {
        client.close_text(*transfer);
    }
    assert!(
        stale.is_err(),
        "a batch write invalidates the shared saved checkpoint before another save begins"
    );
    // Unrelated checkpoints stay warm. Preparing a no-op upload still works
    // from this cached Rope, while publication must detect the removed file.
    std::fs::remove_file(root.path().join("untouched.txt")).unwrap();
    let Reply::Text(super::super::text::Reply::Upload { transfer }) = client
        .call(Command::Text(super::super::text::Command::BeginSave {
            path: path("untouched.txt"),
            base: Some(untouched_revision),
            start: 0,
            end: 0,
            characters: 0,
            bytes: 0,
            encoding: "UTF-8".into(),
        }))
        .unwrap()
    else {
        panic!("untouched checkpoint should remain cached")
    };
    assert!(
        client
            .call(Command::Text(super::super::text::Command::Finish {
                transfer
            }))
            .is_err()
    );
    assert!(!root.path().join("untouched.txt").exists());
    eprintln!(
        "workspace_replace_transport body_bytes={} request_bytes={} response_bytes={}",
        source.len() * 2,
        transferred.0,
        transferred.1
    );
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn workspace_replace_node_rejects_duplicate_targets_before_io_and_keeps_stale_files_atomic() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("stale"), "bar\nfoo").unwrap();
    std::fs::write(root.path().join("good"), "foo").unwrap();
    let client = super::super::tests::connection(root.path(), Arc::default(), Arc::default());
    let result = client.call(Command::Replace(Replace {
        options: options(),
        files: vec![
            File {
                path: path("good"),
                matches: vec![(1, 0, 3)],
            },
            File {
                path: path("good"),
                matches: vec![(1, 0, 3)],
            },
        ],
    }));
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(root.path().join("good")).unwrap(),
        "foo"
    );
    let Reply::Replaced(outcome) = client
        .call(Command::Replace(Replace {
            options: options(),
            files: vec![
                File {
                    path: path("stale"),
                    matches: vec![(1, 0, 3), (2, 0, 3)],
                },
                File {
                    path: path("good"),
                    matches: vec![(1, 0, 3)],
                },
            ],
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        (
            outcome.replacements,
            outcome.files_changed,
            outcome.failures
        ),
        (1, 1, 1)
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("stale")).unwrap(),
        "bar\nfoo"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("good")).unwrap(),
        "foo猫"
    );
    let Reply::Replaced(noop) = client
        .call(Command::Replace(Replace {
            options: Options {
                query: "foo".into(),
                replacement: "foo".into(),
                regex: false,
                ..options()
            },
            files: vec![File {
                path: path("good"),
                matches: vec![(1, 0, 3)],
            }],
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(noop.written_files, 0);
    client.call(Command::Ping).unwrap();
    client.call(Command::Shutdown).unwrap();
}
