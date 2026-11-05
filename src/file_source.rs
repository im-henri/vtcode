//! Native file operations shared by the editor and ACP.
//! A path metadata check alone races replacements, and a normal FIFO open can
//! block before metadata is available. Never consume a nonregular file.
use std::{
    fs, io,
    path::{Component, Path},
};

/// An advisory lock that explicitly releases its lease on drop.
/// A concurrent fork can retain a copy of the open file description even
/// after this owner closes its descriptor.
#[derive(Debug)]
pub struct FileLock(pub(crate) fs::File);

pub use FileLock as ExclusiveFileLock;

impl FileLock {
    pub fn try_acquire(file: fs::File) -> Result<Self, fs::TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }

    pub fn try_acquire_before(
        file: fs::File,
        deadline: std::time::Instant,
    ) -> Result<Self, fs::TryLockError> {
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn acquire_shared(file: fs::File) -> io::Result<Self> {
        file.lock_shared()?;
        Ok(Self(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub fn open_document_source(
    path: &Path,
    allow_directory: bool,
) -> io::Result<(fs::File, fs::Metadata)> {
    open_source(path, allow_directory, true)
}

/// Open a regular file without following a link in the final path component.
pub fn open_regular_file_no_follow(path: &Path) -> io::Result<(fs::File, fs::Metadata)> {
    open_source(path, false, false)
}

/// Open a regular file at an already authorized canonical path. A link
/// substituted anywhere in the path must never redirect the caller's read.
pub(crate) fn open_canonical_regular_file(path: &Path) -> io::Result<(fs::File, fs::Metadata)> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a canonical path",
        ));
    }
    #[cfg(unix)]
    {
        let file = open_unix_canonical_file(path)?;
        source_metadata(file, false)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        let (file, metadata) = open_regular_file_no_follow(path)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other("A link replaced the approved file"));
        }
        let opened = windows_final_path(&file, &mut vec![0; 512])?;
        if opened != path && windows_wide_path(&opened)? != windows_wide_path(path)? {
            return Err(io::Error::other("The approved file path changed"));
        }
        Ok((file, metadata))
    }
    #[cfg(not(any(unix, windows)))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Canonical file opens are unsupported",
    ))
}

#[cfg(unix)]
fn open_unix_canonical_file(path: &Path) -> io::Result<fs::File> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        },
    };

    let file_flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY;
    #[cfg(target_os = "linux")]
    {
        let name = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: open_how is a C struct of unsigned integers. Zeroing also
        // initializes any extension fields for the kernel's size check.
        let mut how: libc::open_how = unsafe { std::mem::zeroed() };
        how.flags = file_flags as u64;
        how.resolve = libc::RESOLVE_NO_SYMLINKS;
        // SAFETY: name and how remain valid for the syscall. No creation mode
        // is needed for this read-only open.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                libc::AT_FDCWD,
                name.as_ptr(),
                &how,
                std::mem::size_of_val(&how),
            )
        };
        if fd >= 0 {
            // SAFETY: the kernel returned a fresh owned descriptor.
            return Ok(unsafe { fs::File::from_raw_fd(fd as libc::c_int) });
        }
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS | libc::EINVAL | libc::EPERM)
        ) {
            return Err(error);
        }
        // Older kernels and some container filters lack openat2.
    }

    let directory_flags = libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    #[cfg(target_os = "linux")]
    let directory_flags = directory_flags | libc::O_PATH;
    let mut current = fs::OpenOptions::new()
        .read(true)
        .custom_flags(directory_flags)
        .open("/")?;
    let mut parts = path.components().skip(1).peekable();
    while let Some(Component::Normal(part)) = parts.next() {
        let name = CString::new(part.as_bytes())?;
        let flags = if parts.peek().is_some() {
            directory_flags
        } else {
            file_flags | libc::O_NOFOLLOW
        };
        // SAFETY: current owns the directory descriptor and name holds one
        // terminated path component. Every open rejects symlinks.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        current = unsafe { fs::File::from_raw_fd(fd) };
    }
    Ok(current)
}

fn open_source(
    path: &Path,
    allow_directory: bool,
    follow_links: bool,
) -> io::Result<(fs::File, fs::Metadata)> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | if follow_links { 0 } else { libc::O_NOFOLLOW });
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        let directory = if allow_directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
        let link = if follow_links {
            0
        } else {
            FILE_FLAG_OPEN_REPARSE_POINT
        };
        options.custom_flags(directory | link);
    }
    source_metadata(options.open(path)?, allow_directory)
}

fn source_metadata(file: fs::File, allow_directory: bool) -> io::Result<(fs::File, fs::Metadata)> {
    let metadata = file.metadata()?;
    if !metadata.is_file() && !(allow_directory && metadata.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a regular file",
        ));
    }
    Ok((file, metadata))
}

#[cfg(windows)]
pub fn windows_final_path(file: &fs::File, buffer: &mut Vec<u16>) -> io::Result<std::path::PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    loop {
        // SAFETY: the file owns the handle and buffer has initialized writable
        // elements. Normalized names resolve ancestor junctions as well.
        let size = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        if (size as usize) < buffer.len() {
            return Ok(std::ffi::OsString::from_wide(&buffer[..size as usize]).into());
        }
        buffer.resize(size as usize + 1, 0);
    }
}

#[cfg(windows)]
fn windows_wide_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::{os::windows::ffi::OsStrExt, path::Prefix};

    let absolute = std::path::absolute(path)?;
    let wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Path contains a null character",
        ));
    }
    // Retain non-Unicode filenames and long or UNC paths without resolving links.
    let mut wide = match absolute.components().next() {
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

/// Move the directory entry itself, without following symlinks or replacing a
/// destination that another process may have created since it was last checked.
pub fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    if source == destination {
        return std::fs::symlink_metadata(source).map(|_| ());
    }
    #[cfg(windows)]
    {
        let source = windows_wide_path(source)?;
        let destination = windows_wide_path(destination)?;
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

/// Return the device or volume and file identifier of the opened entry.
pub(crate) fn file_identity(_file: &fs::File, _metadata: &fs::Metadata) -> Option<(u64, u128)> {
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        Some((_metadata.dev(), u128::from(_metadata.ino())))
    };
    #[cfg(windows)]
    let identity = windows_file_identity(_file);
    #[cfg(not(any(unix, windows)))]
    let identity = None;
    identity.filter(|(_, file)| *file != 0)
}

/// Return the volume and file identifier for an open Windows handle.
#[cfg(windows)]
pub fn windows_file_identity(file: &fs::File) -> Option<(u64, u128)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    };
    let handle = file.as_raw_handle();
    let mut id = FILE_ID_INFO::default();
    // SAFETY: The file owns the handle, and the output structure matches the
    // information class and size passed to the call.
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0 {
        return Some((
            id.VolumeSerialNumber,
            u128::from_le_bytes(id.FileId.Identifier),
        ));
    }
    // Some older or remote filesystems expose only the legacy 64-bit index.
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: The handle remains open and info is writable for the entire call.
    (unsafe { GetFileInformationByHandle(handle, &mut info) } != 0).then(|| {
        let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
        (u64::from(info.dwVolumeSerialNumber), u128::from(index))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[cfg(unix)]
    #[test]
    fn dropping_a_shared_lease_releases_it_even_with_an_inherited_descriptor() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery.lock");
        let file = fs::File::create(&path).unwrap();
        let inherited = file.try_clone().unwrap();
        let lease = FileLock::acquire_shared(file).unwrap();
        let contender = || FileLock::try_acquire(fs::File::open(&path).unwrap());
        assert!(matches!(contender(), Err(fs::TryLockError::WouldBlock)));

        drop(lease);

        assert!(contender().is_ok());
        drop(inherited);
    }

    #[test]
    fn regular_files_and_optional_directories_use_the_opened_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        fs::write(&path, "λ\r\n").unwrap();
        for allow_directory in [false, true] {
            let (mut file, metadata) = open_document_source(&path, allow_directory).unwrap();
            assert!(metadata.is_file());
            assert_eq!(metadata.len(), 4);
            let mut text = String::new();
            file.read_to_string(&mut text).unwrap();
            assert_eq!(text, "λ\r\n");
        }
        let (_, metadata) = open_document_source(directory.path(), true).unwrap();
        assert!(metadata.is_dir());
        assert!(open_document_source(directory.path(), false).is_err());
    }
}
