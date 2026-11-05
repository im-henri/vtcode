use super::*;

fn save(name: &str, root: &Path) -> Mutation {
    Mutation::Save {
        name: name.into(),
        scope: Scope::Local,
        roots: vec![Location::local(root, &crate::portable::base_dir())],
        active: 0,
        replace: None,
    }
}

#[test]
fn persistence_update_rename_delete_and_duplicate_names_preserve_other_sets() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sets.json");
    let first = mutate(&path, save("Backend", temp.path())).unwrap().sets[0].id;
    let second = mutate(&path, save("Frontend", temp.path())).unwrap().sets[1].id;
    assert!(mutate(&path, save(" backend ", temp.path())).is_err());
    mutate(
        &path,
        Mutation::Rename {
            id: first,
            scope: Scope::Local,
            name: "Infrastructure λ🦀".into(),
        },
    )
    .unwrap();
    let mut update = save("Infrastructure λ🦀", temp.path());
    if let Mutation::Save { replace, .. } = &mut update {
        *replace = Some(first);
    }
    mutate(&path, update).unwrap();
    let catalog = mutate(
        &path,
        Mutation::Delete {
            id: second,
            scope: Scope::Local,
        },
    )
    .unwrap();
    assert_eq!(catalog.sets.len(), 1);
    assert_eq!(load(&path).unwrap().sets[0].id, first);
    mutate(
        &path,
        Mutation::Delete {
            id: first,
            scope: Scope::Local,
        },
    )
    .unwrap();
    let next = mutate(&path, save("New", temp.path())).unwrap().sets[0].id;
    assert!(
        next > second,
        "deleted IDs must not be reused by stale catalog readers"
    );
}

#[test]
fn concurrent_catalog_writers_merge_without_losing_sets() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sets.json");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let jobs: Vec<_> = (0..8)
        .map(|index| {
            let path = path.clone();
            let root = temp.path().to_owned();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                mutate(&path, save(&format!("Set {index}"), &root)).unwrap();
            })
        })
        .collect();
    for job in jobs {
        job.join().unwrap();
    }
    let sets = load(&path).unwrap().sets;
    assert_eq!(sets.len(), 8);
    assert_eq!(
        sets.iter()
            .map(|set| set.id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        8
    );
}

#[cfg(unix)]
#[test]
fn workspace_catalog_rejects_a_fifo_without_waiting_for_a_writer() {
    use std::{
        fs::OpenOptions,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        sync::mpsc,
        time::{Duration, Instant},
    };

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sets.json");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the path is a terminated name in a disposable directory.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let (done, result) = mpsc::channel();
    let read_path = path.clone();
    let reader = std::thread::spawn(move || {
        let _ = done.send(load(&read_path));
    });
    let early = result.recv_timeout(Duration::from_secs(2));
    if early.is_err() {
        // Unblock a regressed reader before joining the thread.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
            {
                Ok(peer) => {
                    drop(peer);
                    break;
                }
                Err(error)
                    if error.raw_os_error() == Some(libc::ENXIO) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("cannot unblock catalog reader: {error}"),
            }
        }
        result.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_err();
    }
    reader.join().unwrap();
    assert!(early.expect("catalog load waited for a FIFO writer").is_err());
}

#[test]
fn malformed_newer_and_invalid_mutations_never_overwrite_catalogs() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("sets.json");
    for contents in ["not JSON", r#"{"version":999,"sets":[]}"#] {
        std::fs::write(&path, contents).unwrap();
        assert!(mutate(&path, save("New", temp.path())).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    }
    std::fs::remove_file(&path).unwrap();
    mutate(&path, save("Original", temp.path())).unwrap();
    let before = std::fs::read(&path).unwrap();
    let invalid = Mutation::Save {
        name: "Bad".into(),
        scope: Scope::Local,
        roots: vec![Location::Remote {
            uri: "file:///wrong".into(),
        }],
        active: 0,
        replace: None,
    };
    assert!(mutate(&path, invalid).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for name in ["", " \t ", "bad\nname"] {
        assert!(valid_name(name).is_err());
    }
}

#[test]
fn portable_paths_follow_the_executable_and_native_names_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let old = temp.path().join("before");
    let new = temp.path().join("after");
    let saved = Location::local(&old.join("projects/λ🦀"), &old);
    assert_eq!(saved.local_path(&new).unwrap(), new.join("projects/λ🦀"));
    #[cfg(windows)]
    let odd = {
        use std::os::windows::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_wide(&[67, 58, 92, 0xd800]))
    };
    #[cfg(unix)]
    let odd = {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]))
    };
    let location = Location::local(&odd, &old);
    let encoded = serde_json::to_vec(&location).unwrap();
    let decoded: Location = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.local_path(&new).unwrap(), odd);
}
