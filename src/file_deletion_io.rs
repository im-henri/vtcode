//! Remove entries through anchored OS handles. A directory-to-link swap during
//! traversal must never redirect an unlink into a different directory tree.
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

#[cfg(unix)]
pub(super) struct Root {
    directories: Vec<(PathBuf, File)>,
}

#[cfg(unix)]
impl Root {
    pub(super) fn new(target: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("Invalid deletion path"))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)?;
        Ok(Self {
            directories: vec![(parent.to_owned(), file)],
        })
    }

    fn parent_fd(&mut self, parent: &Path) -> io::Result<std::os::fd::RawFd> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        if let Some((_, file)) = self.directories.iter().find(|(path, _)| path == parent) {
            return Ok(file.as_raw_fd());
        }
        let (path, file) = self
            .directories
            .iter()
            .filter(|(path, _)| parent.starts_with(path))
            .max_by_key(|(path, _)| path.components().count())
            .ok_or_else(|| io::Error::other("Deletion path left the selected directory"))?;
        let mut fd = file.as_raw_fd();
        let mut path = path.clone();
        let suffix = parent.strip_prefix(&path).unwrap().to_owned();
        for component in suffix.components() {
            if !matches!(component, std::path::Component::Normal(_)) {
                return Err(io::Error::other("Invalid deletion path component"));
            }
            let name = std::ffi::CString::new(component.as_os_str().as_bytes())?;
            // SAFETY: fd belongs to a retained directory handle. The name is
            // one terminated component; O_NOFOLLOW rejects swapped symlinks.
            let child = unsafe {
                libc::openat(
                    fd,
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if child < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openat returned a fresh owned descriptor.
            let child = unsafe { File::from_raw_fd(child) };
            path.push(component);
            // Keep the base anchor and a bounded cache of recently visited
            // parents. Siblings reuse their handle instead of reopening paths.
            if self.directories.len() == 16 {
                self.directories.remove(1);
            }
            fd = child.as_raw_fd();
            self.directories.push((path.clone(), child));
        }
        Ok(fd)
    }

    pub(super) fn remove(&mut self, entry: &walkdir::DirEntry) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let parent = entry
            .path()
            .parent()
            .ok_or_else(|| io::Error::other("Invalid deletion path"))?;
        let fd = self.parent_fd(parent)?;
        let name = std::ffi::CString::new(entry.file_name().as_bytes())?;
        let flags = if entry.file_type().is_dir() {
            libc::AT_REMOVEDIR
        } else {
            0
        };
        // SAFETY: the parent descriptor is retained and the leaf is a single
        // component. unlinkat removes directory entries, never link targets.
        if unsafe { libc::unlinkat(fd, name.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
pub(super) struct Root {
    _parent: File,
    selected: PathBuf,
    buffer: Vec<u16>,
}

#[cfg(windows)]
fn final_path(file: &File, buffer: &mut Vec<u16>) -> io::Result<PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    loop {
        // SAFETY: file owns the handle and buffer has initialized writable
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
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..size as usize],
            )));
        }
        buffer.resize(size as usize + 1, 0);
    }
}

#[cfg(windows)]
impl Root {
    pub(super) fn new(target: &Path) -> io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::*;
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::other("Invalid deletion path"))?;
        let file = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(parent)?;
        let mut buffer = vec![0; 512];
        let selected = final_path(&file, &mut buffer)?.join(target.file_name().unwrap());
        Ok(Self {
            _parent: file,
            selected,
            buffer,
        })
    }

    pub(super) fn remove(&mut self, entry: &walkdir::DirEntry) -> io::Result<()> {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::*;
        let file = std::fs::OpenOptions::new()
            .access_mode(DELETE | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(entry.path())?;
        let resolved = final_path(&file, &mut self.buffer)?;
        if !resolved
            .ancestors()
            .any(|ancestor| crate::app::App::same_open_path(ancestor, &self.selected))
        {
            return Err(io::Error::other(
                "Directory changed during deletion; refusing to follow a link outside the selection",
            ));
        }
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: the structure and live handle remain valid for this call.
        // Delete by this verified handle, so a subsequent rename/link swap
        // cannot redirect removal to another file. It closes before the parent.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of_val(&disposition) as u32,
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
