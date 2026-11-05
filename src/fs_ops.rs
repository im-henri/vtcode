//! Shared filesystem operations with explicit replacement semantics.
use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) use vtcode::file_source::{open_document_source, rename_no_replace};

/// Open an advisory lock file without following links or truncating it.
pub(crate) fn open_lock_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Lock path must be a regular file",
        ));
    }
    Ok(file)
}

const COPY_TEMP_PREFIX: &str = ".vtcode-copy-";
const COPY_TEMP_RANDOM_LEN: usize = 12;
const COPY_CHUNK_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const COPY_TEMP_GIT_IGNORE: &str = ".vtcode-copy-????????????/";

pub(crate) fn is_copy_temporary_name(name: &OsStr) -> bool {
    name.as_encoded_bytes()
        .strip_prefix(COPY_TEMP_PREFIX.as_bytes())
        .is_some_and(|suffix| {
            suffix.len() == COPY_TEMP_RANDOM_LEN && suffix.iter().all(u8::is_ascii_alphanumeric)
        })
}

pub(crate) fn is_copy_temporary_path(path: &Path) -> bool {
    path.components()
        .any(|part| is_copy_temporary_name(part.as_os_str()))
}

/// Copy a tree without merging into existing entries or following directory
/// links. Publish only the complete copy, even if the operation is cancelled
/// or another process creates the destination while it is being staged.
pub(crate) fn copy_no_replace(
    source: &Path,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> io::Result<()> {
    let check = || {
        if cancelled() {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(())
        }
    };
    check()?;
    match fs::symlink_metadata(destination) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(source)?;
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if metadata.is_dir() && fs::canonicalize(parent)?.starts_with(fs::canonicalize(source)?) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Cannot copy a folder into itself",
        ));
    }
    let mut builder = tempfile::Builder::new();
    builder
        .prefix(COPY_TEMP_PREFIX)
        .rand_bytes(COPY_TEMP_RANDOM_LEN);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(parent)?;
    let item = staging.path().join("item");
    #[cfg(unix)]
    let mut restricted_dirs = Vec::new();
    let result = (|| {
        if metadata.is_dir() {
            fs::create_dir(&item)?;
            let mut stack = vec![(fs::read_dir(source)?, item.clone(), metadata.permissions())];
            while let Some((entries, target, _)) = stack.last_mut() {
                check()?;
                if let Some(entry) = entries.next() {
                    let entry = entry?;
                    let source = entry.path();
                    let destination = target.join(entry.file_name());
                    let metadata = entry.metadata()?;
                    if metadata.is_dir() {
                        fs::create_dir(&destination)?;
                        stack.push((fs::read_dir(source)?, destination, metadata.permissions()));
                    } else {
                        copy_leaf(&source, &destination, metadata.file_type(), &check)?;
                    }
                } else {
                    let (_, path, permissions) = stack.pop().unwrap();
                    #[cfg(unix)]
                    let restricts_cleanup = {
                        use std::os::unix::fs::PermissionsExt;
                        permissions.mode() & 0o700 != 0o700
                    };
                    fs::set_permissions(&path, copied_permissions(permissions))?;
                    #[cfg(unix)]
                    if restricts_cleanup {
                        restricted_dirs.push(path);
                    }
                }
            }
        } else {
            copy_leaf(source, &item, metadata.file_type(), &check)?;
        }
        check()?;
        rename_no_replace(&item, destination)
    })();
    #[cfg(unix)]
    if result.is_err() {
        use std::os::unix::fs::PermissionsExt;
        // Restore access top-down so TempDir can remove a failed copy of a
        // tree that contains read-only directories.
        for path in restricted_dirs.into_iter().rev() {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        }
    }
    result
}

fn copy_leaf(
    source: &Path,
    destination: &Path,
    file_type: fs::FileType,
    check: &impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    if file_type.is_symlink() {
        let target = fs::read_link(source)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, destination);
        #[cfg(windows)]
        {
            use std::os::windows::fs::{FileTypeExt, symlink_dir, symlink_file};
            return if file_type.is_symlink_dir() {
                symlink_dir(target, destination)
            } else {
                symlink_file(target, destination)
            };
        }
    }

    use io::Read;
    let (mut input, source_metadata) = vtcode::file_source::open_regular_file_no_follow(source)?;
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    loop {
        check()?;
        if io::copy(&mut input.by_ref().take(COPY_CHUNK_BYTES), &mut output)? < COPY_CHUNK_BYTES {
            break;
        }
    }
    output.set_permissions(copied_permissions(source_metadata.permissions()))
}

fn copied_permissions(permissions: fs::Permissions) -> fs::Permissions {
    #[cfg(unix)]
    let permissions = {
        use std::os::unix::fs::PermissionsExt;
        // Copies are newly owned. Setuid/setgid could grant the owner's or
        // destination group's privileges to an executable from another user.
        fs::Permissions::from_mode(permissions.mode() & 0o1777)
    };
    permissions
}

pub(crate) fn paste_items(
    sources: Vec<PathBuf>,
    directory: &Path,
    cut: bool,
    cancelled: impl Fn() -> bool,
) -> io::Result<crate::background::FilesystemActionOutcome> {
    if cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if !fs::metadata(directory)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Paste destination is not a directory",
        ));
    }
    let mut outcome = crate::background::FilesystemActionOutcome::default();
    let mut failed = 0;
    let mut last_error = None;
    let mut stopped = false;
    for source in sources {
        if cancelled() {
            stopped = true;
            break;
        }
        let result =
            paste_destination(&source, directory, cut, &cancelled).and_then(|destination| {
                if cancelled() {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                if cut {
                    rename_no_replace(&source, &destination)?;
                } else {
                    copy_no_replace(&source, &destination, &cancelled)?;
                }
                Ok(destination)
            });
        match result {
            Ok(destination) => {
                if cut {
                    if source != destination {
                        outcome.renames.push((source.clone(), destination.clone()));
                    }
                    outcome.completed_cut_paths.push(source);
                }
                outcome.pasted_paths.push(destination);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted && cancelled() => {
                stopped = true;
                break;
            }
            Err(error) => {
                failed += 1;
                last_error = Some(io::Error::new(
                    error.kind(),
                    format!("{}: {error}", source.display()),
                ));
            }
        }
    }
    let processed = outcome.pasted_paths.len();
    if processed == 0 {
        return Err(last_error.unwrap_or_else(|| {
            if stopped {
                io::ErrorKind::Interrupted.into()
            } else {
                io::Error::other("Nothing to paste")
            }
        }));
    }
    let verb = if cut { "Moved" } else { "Pasted" };
    outcome.message = if processed == 1 && failed == 0 && !stopped {
        format!("{verb}: {}", outcome.pasted_paths[0].display())
    } else {
        format!("{verb} {processed} items")
    };
    if let Some(error) = last_error {
        outcome
            .message
            .push_str(&format!(". {failed} failed ({error})"));
    }
    if stopped {
        outcome.message.push_str(". Paste cancelled");
    }
    outcome.warning = failed != 0 || stopped;
    Ok(outcome)
}

fn paste_destination(
    source: &Path,
    directory: &Path,
    cut: bool,
    cancelled: &impl Fn() -> bool,
) -> io::Result<PathBuf> {
    let name = source
        .file_name()
        .ok_or_else(|| io::Error::other("Cannot paste a filesystem root"))?;
    let destination = directory.join(name);
    let available = |path: &Path| {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error),
        }
    };
    if (cut && source == destination) || available(&destination)? {
        return Ok(destination);
    }
    // Keep the native spelling, including names that cannot round-trip UTF-8.
    let stem = source.file_stem().unwrap_or(name);
    let extension = source.extension();
    for suffix in 1..=10_000 {
        let mut name = stem.to_os_string();
        name.push(format!(" ({suffix})"));
        if let Some(extension) = extension {
            name.push(".");
            name.push(extension);
        }
        let destination = directory.join(name);
        if available(&destination)? {
            return Ok(destination);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not choose an unused copy name",
    ))
}

#[cfg(test)]
mod copy_tests {
    use super::*;
    use std::cell::Cell;

    #[cfg(unix)]
    #[test]
    fn copying_executables_preserves_normal_mode_without_copying_privilege_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("copy");
        fs::write(&source, "executable content").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o6751)).unwrap();
        copy_no_replace(&source, &destination, || false).unwrap();
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o7777,
            0o6751
        );
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o7777,
            0o751
        );
    }

    #[cfg(unix)]
    #[test]
    fn paste_collision_suffix_preserves_native_filename_bytes() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join(OsStr::from_bytes(b"name\xff.ext\xfe"));
        let expected = dir.path().join(OsStr::from_bytes(b"name\xff (1).ext\xfe"));
        fs::write(&source, "text").unwrap();
        let outcome = paste_items(vec![source], dir.path(), false, || false).unwrap();
        assert_eq!(outcome.pasted_paths, [expected.clone()]);
        assert_eq!(fs::read_to_string(expected).unwrap(), "text");
    }

    #[test]
    fn cancelling_during_a_file_copy_removes_the_unpublished_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::File::create(&source)
            .unwrap()
            .set_len(COPY_CHUNK_BYTES + 1)
            .unwrap();
        let destination = dir.path().join("copy");
        let checks = Cell::new(0);
        let result = copy_no_replace(&source, &destination, || {
            checks.set(checks.get() + 1);
            checks.get() == 3
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn publishing_a_copy_preserves_a_destination_created_during_staging() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, "original").unwrap();
        let destination = dir.path().join("copy");
        let checks = Cell::new(0);
        let result = copy_no_replace(&source, &destination, || {
            checks.set(checks.get() + 1);
            if checks.get() == 3 {
                fs::write(&destination, "concurrent").unwrap();
            }
            false
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(destination).unwrap(), "concurrent");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_tree_copy_cleans_up_readonly_directories() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let child = source.join("readonly");
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("file"), "original").unwrap();
        fs::set_permissions(&child, fs::Permissions::from_mode(0o500)).unwrap();
        let destination = dir.path().join("copy");
        let checks = Cell::new(0);
        let result = copy_no_replace(&source, &destination, || {
            checks.set(checks.get() + 1);
            checks.get() == 6
        });
        fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn copy_rejects_descendants_collisions_and_pre_cancelled_operations() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("code.rs"), "keep").unwrap();
        assert!(copy_no_replace(&source, &source.join("copy"), || false).is_err());
        assert!(!source.join("copy").exists());
        let destination = dir.path().join("copy");
        copy_no_replace(&source, &destination, || false).unwrap();
        std::fs::write(destination.join("code.rs"), "preserve").unwrap();
        assert!(copy_no_replace(&source, &destination, || false).is_err());
        assert_eq!(
            std::fs::read_to_string(destination.join("code.rs")).unwrap(),
            "preserve"
        );
        assert!(copy_no_replace(&source, &dir.path().join("cancelled"), || true).is_err());
        assert!(!dir.path().join("cancelled").exists());
    }
    #[cfg(unix)]
    #[test]
    fn copy_preserves_directory_symlink_cycles_without_following_them() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink(".", source.join("loop")).unwrap();
        let destination = dir.path().join("copy");
        copy_no_replace(&source, &destination, || false).unwrap();
        assert_eq!(
            std::fs::read_link(destination.join("loop")).unwrap(),
            std::path::PathBuf::from(".")
        );
    }

    #[cfg(windows)]
    #[test]
    fn copy_preserves_the_type_of_a_dangling_directory_link() {
        use std::os::windows::fs::{FileTypeExt, symlink_dir};

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("copy");
        if let Err(error) = symlink_dir("missing-directory", &source) {
            // Windows hosts without Developer Mode or SeCreateSymbolicLinkPrivilege.
            if error.raw_os_error() == Some(1314) {
                return;
            }
            panic!("could not create a directory link: {error}");
        }
        copy_no_replace(&source, &destination, || false).unwrap();
        assert!(
            fs::symlink_metadata(&destination)
                .unwrap()
                .file_type()
                .is_symlink_dir()
        );
        assert_eq!(
            fs::read_link(&destination).unwrap(),
            fs::read_link(&source).unwrap()
        );
    }
}
