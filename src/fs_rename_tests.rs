use super::*;
use std::fs;

pub(super) fn unavailable() -> io::Error {
    #[cfg(target_os = "linux")]
    return io::Error::from_raw_os_error(libc::EINVAL);
    #[cfg(not(target_os = "linux"))]
    io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem lacks RENAME_NOREPLACE",
    )
}

fn move_entry(source: &Path, destination: &Path) -> io::Result<()> {
    with_primary(source, destination, || Err(unavailable()))
}

#[test]
fn unsupported_rename_option_moves_files_and_directories_with_their_contents() {
    for directory in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("old_資料.tcl");
        let destination = root.path().join("new_資料.tcl");
        let contents = if directory {
            fs::create_dir(&source).unwrap();
            source.join("child.tcl")
        } else {
            source.clone()
        };
        fs::write(&contents, "keep contents\n").unwrap();
        move_entry(&source, &destination).unwrap();
        assert!(!source.exists());
        let contents = if directory {
            destination.join("child.tcl")
        } else {
            destination
        };
        assert_eq!(fs::read_to_string(contents).unwrap(), "keep contents\n");
    }
}

#[test]
fn fallback_refuses_existing_files_and_empty_or_nonempty_directories() {
    for source_directory in [false, true] {
        for destination_kind in [0, 1, 2] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            let destination = root.path().join("destination");
            if source_directory {
                fs::create_dir(&source).unwrap();
            } else {
                fs::write(&source, "source contents").unwrap();
            }
            if destination_kind == 0 {
                fs::write(&destination, "destination contents").unwrap();
            } else {
                fs::create_dir(&destination).unwrap();
                if destination_kind == 2 {
                    fs::write(destination.join("child"), "child").unwrap();
                }
            }
            assert_eq!(
                move_entry(&source, &destination).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert!(source.exists());
            match destination_kind {
                0 => assert_eq!(
                    fs::read_to_string(destination).unwrap(),
                    "destination contents"
                ),
                1 => assert_eq!(fs::read_dir(destination).unwrap().count(), 0),
                _ => assert_eq!(
                    fs::read_to_string(destination.join("child")).unwrap(),
                    "child"
                ),
            }
        }
    }
}

#[test]
fn racing_fallback_moves_preserve_the_losing_source_even_for_empty_directories() {
    for directory in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let sources = [root.path().join("first"), root.path().join("second")];
        let destination = root.path().join("destination");
        for (index, source) in sources.iter().enumerate() {
            if directory {
                fs::create_dir(source).unwrap();
            } else {
                fs::write(source, index.to_string()).unwrap();
            }
        }
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let jobs: Vec<_> = sources
                .iter()
                .map(|source| {
                    let barrier = &barrier;
                    let destination = &destination;
                    scope.spawn(move || {
                        barrier.wait();
                        move_entry(source, destination)
                    })
                })
                .collect();
            jobs.into_iter()
                .map(|job| job.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let winner = results.iter().position(|result| result.is_ok()).unwrap();
        assert!(!sources[winner].exists());
        assert!(sources[1 - winner].exists());
        assert_eq!(
            results[1 - winner].as_ref().unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        if !directory {
            assert_eq!(fs::read_to_string(destination).unwrap(), winner.to_string());
            assert_eq!(
                fs::read_to_string(&sources[1 - winner]).unwrap(),
                (1 - winner).to_string()
            );
        }
    }
}

#[test]
fn native_errors_are_preserved_and_do_not_start_a_fallback() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    fs::write(&source, "keep").unwrap();
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::AlreadyExists,
        io::ErrorKind::InvalidInput,
        io::ErrorKind::CrossesDevices,
        io::ErrorKind::Other,
    ] {
        let error = with_primary(&source, &destination, || {
            Err(io::Error::new(kind, "original error"))
        })
        .unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(error.to_string(), "original error");
        assert!(source.exists());
        assert!(!destination.exists());
    }
}

#[test]
fn same_path_and_invalid_directory_moves_keep_their_normal_semantics() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    move_entry(&source, &source).unwrap();
    assert!(move_entry(&source, &source.join("child")).is_err());
    assert!(source.is_dir());
    assert!(!source.join("child").exists());
    let missing = root.path().join("missing");
    assert!(move_entry(&missing, &missing).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn linux_flag_rejection_errors_all_use_the_compatibility_path() {
    let root = tempfile::tempdir().unwrap();
    for code in [libc::EINVAL, libc::ENOSYS, libc::EOPNOTSUPP] {
        let source = root.path().join(format!("source-{code}"));
        let destination = root.path().join(format!("destination-{code}"));
        fs::write(&source, "keep").unwrap();
        with_primary(&source, &destination, || {
            Err(io::Error::from_raw_os_error(code))
        })
        .unwrap();
        assert_eq!(fs::read_to_string(destination).unwrap(), "keep");
        assert!(!source.exists());
    }
}

#[cfg(unix)]
#[test]
fn fallback_moves_dangling_symlinks_and_refuses_existing_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    std::os::unix::fs::symlink("missing", &source).unwrap();
    move_entry(&source, &destination).unwrap();
    assert_eq!(fs::read_link(&destination).unwrap(), Path::new("missing"));
    assert!(fs::symlink_metadata(&source).is_err());
    fs::write(&source, "keep").unwrap();
    assert_eq!(
        move_entry(&source, &destination).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(source).unwrap(), "keep");
}
