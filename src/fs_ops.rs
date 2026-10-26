//! Shared filesystem operations with explicit replacement semantics.
use std::path::Path;

pub(crate) use vtcode::file_source::open_document_source;

/// Copy a tree without merging into existing entries or following directory
/// links. Iterative traversal cannot overflow the stack on deeply nested trees.
pub(crate) fn copy_no_replace(
    source: &Path,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> std::io::Result<()> {
    use std::{fs, io};
    let check = || {
        if cancelled() {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            Ok(())
        }
    };
    check()?;
    fn leaf(source: &Path, destination: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(source)?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(source)?;
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(target, destination)
            }
            #[cfg(windows)]
            {
                if source.is_dir() {
                    std::os::windows::fs::symlink_dir(target, destination)
                } else {
                    std::os::windows::fs::symlink_file(target, destination)
                }
            }
        } else {
            let (mut input, source_metadata) = open_document_source(source, false)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            io::copy(&mut input, &mut output)?;
            output.set_permissions(source_metadata.permissions())
        }
    }
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.is_dir() {
        return leaf(source, destination);
    }
    let source_root = fs::canonicalize(source)?;
    let parent = fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| io::Error::other("Copy destination has no parent"))?,
    )?;
    if parent.starts_with(&source_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Cannot copy a folder into itself",
        ));
    }
    fs::create_dir(destination)?;
    let mut stack = vec![(
        fs::read_dir(source)?,
        destination.to_owned(),
        metadata.permissions(),
    )];
    while let Some((entries, target, _)) = stack.last_mut() {
        check()?;
        match entries.next() {
            Some(entry) => {
                let entry = entry?;
                let destination = target.join(entry.file_name());
                if entry.file_type()?.is_dir() {
                    fs::create_dir(&destination)?;
                    stack.push((
                        fs::read_dir(entry.path())?,
                        destination,
                        entry.metadata()?.permissions(),
                    ));
                } else {
                    leaf(&entry.path(), &destination)?;
                }
            }
            None => {
                let (_, destination, permissions) = stack.pop().unwrap();
                fs::set_permissions(destination, permissions)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod copy_tests {
    use super::*;
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
}

/// Move the directory entry itself, without following symlinks or replacing a
/// destination that another process may have created since it was last checked.
pub(crate) fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    if source == destination {
        return std::fs::symlink_metadata(source).map(|_| ());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::path::{Component, Prefix};
        fn wide_path(path: &Path) -> std::io::Result<Vec<u16>> {
            let absolute = std::path::absolute(path)?;
            let wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
            if wide.contains(&0) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Path contains a null character",
                ));
            }
            // Match std::fs support for long paths, retaining non-Unicode
            // filenames and UNC paths without resolving a source symlink.
            let mut wide: Vec<u16> = match absolute.components().next() {
                Some(Component::Prefix(prefix)) => match prefix.kind() {
                    Prefix::Disk(_) => r"\\?\".encode_utf16().chain(wide).collect(),
                    Prefix::UNC(_, _) => r"\\?\UNC\"
                        .encode_utf16()
                        .chain(wide.into_iter().skip(2))
                        .collect(),
                    _ => wide,
                },
                _ => wide,
            };
            wide.push(0);
            Ok(wide)
        }
        let source = wide_path(source)?;
        let destination = wide_path(destination)?;
        // SAFETY: both paths are terminated, contain no embedded NULs, and
        // remain alive for the call. No replacement/cross-volume copy flags.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                0,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        // SAFETY: CString owns both terminated paths for the whole call.
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "Safe rename is unsupported on this platform",
        ))
    }
}
