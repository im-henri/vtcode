//! Explorer moves and staged-copy publication on filesystems without
//! renameat2(RENAME_NOREPLACE), including older/network Linux filesystems.
use std::{io, path::Path};

pub(crate) fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    return with_primary(source, destination, || {
        vtcode::file_source::rename_no_replace(source, destination)
    });
    #[cfg(not(target_os = "linux"))]
    vtcode::file_source::rename_no_replace(source, destination)
}

#[cfg(any(target_os = "linux", test))]
fn unsupported_option(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(target_os = "linux")]
    if matches!(
        error.raw_os_error(),
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
    ) {
        return true;
    }
    false
}

#[cfg(any(target_os = "linux", test))]
pub(super) fn with_primary(
    source: &Path,
    destination: &Path,
    primary: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if source == destination {
        return std::fs::symlink_metadata(source).map(|_| ());
    }
    match primary() {
        Err(error) if unsupported_option(&error) => portable_move(source, destination),
        result => result,
    }
}

#[cfg(any(target_os = "linux", test))]
fn destination_absent(destination: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(destination) {
        Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(any(target_os = "linux", test))]
fn portable_move(source: &Path, destination: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        // POSIX cannot hard-link directories. Use the checked rename fallback
        // used by portable filesystem tools. Serialize our fallbacks so two
        // requests cannot replace each other's newly moved empty directory.
        // External processes can still race this check on filesystems without
        // atomic no-replace support; never claim the syscall's full guarantee.
        static DIRECTORY_MOVES: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = DIRECTORY_MOVES
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        destination_absent(destination)?;
        return std::fs::rename(source, destination);
    }
    destination_absent(destination)?;
    // link(2) reserves the destination atomically without replacing an entry,
    // and on Linux links the symlink itself, including dangling symlinks.
    std::fs::hard_link(source, destination)?;
    std::fs::remove_file(source).map_err(|error| {
        // Keep both names if unlink fails. Removing the destination here could
        // delete another process's replacement, or discard its recent edits.
        io::Error::new(
            error.kind(),
            format!("Destination was created, but the source could not be removed: {error}"),
        )
    })
}

#[cfg(test)]
#[path = "fs_rename_tests.rs"]
mod tests;
