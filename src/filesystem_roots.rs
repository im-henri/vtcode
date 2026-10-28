//! Filesystem entry points for the folder picker, collected on its I/O worker.
use std::{io, path::PathBuf};

pub(crate) fn list() -> io::Result<Vec<(String, PathBuf)>> {
    #[cfg(windows)]
    {
        // Enumerating the drive mask does not probe removable media or wait on
        // disconnected network drives. Opening a drive reports its own errors.
        let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        if mask == 0 { return Err(io::Error::last_os_error()); }
        Ok((b'A'..=b'Z').filter(|letter| mask & (1 << (letter - b'A')) != 0)
            .map(|letter| (format!("Disk ({}:)", letter as char),
                PathBuf::from(format!("{}:\\", letter as char))))
            .collect())
    }
    #[cfg(not(windows))]
    {
        let mut paths = vec![PathBuf::from("/")];
        #[cfg(target_os = "linux")]
        if let Ok(table) = std::fs::read("/proc/self/mountinfo") {
            paths.extend(linux_mounts(&table));
        }
        #[cfg(target_os = "macos")]
        paths.extend(macos_mounts()?);
        paths.sort();
        paths.dedup();
        Ok(paths.into_iter().filter(|path| path.is_dir()).map(|path| {
            let mut display = String::new();
            for ch in path.to_string_lossy().chars() {
                if ch.is_control() { display.extend(ch.escape_default()); } else { display.push(ch); }
            }
            (format!("File system ({display})"), path)
        }).collect())
    }
}

#[cfg(target_os = "linux")]
fn linux_mounts(table: &[u8]) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    table.split(|byte| *byte == b'\n').filter_map(|line| {
        let encoded = line.split(|byte| *byte == b' ').nth(4)?;
        let mut bytes = Vec::with_capacity(encoded.len());
        let mut at = 0;
        while at < encoded.len() {
            if encoded[at] == b'\\' {
                let octal = encoded.get(at + 1..at + 4)?;
                if !octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) { return None; }
                let value = ((octal[0] - b'0') as u16) * 64
                    + ((octal[1] - b'0') as u16) * 8 + (octal[2] - b'0') as u16;
                bytes.push(u8::try_from(value).ok()?);
                at += 4;
            } else {
                bytes.push(encoded[at]);
                at += 1;
            }
        }
        let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
        path.is_absolute().then_some(path)
    }).collect()
}

#[cfg(target_os = "macos")]
fn macos_mounts() -> io::Result<Vec<PathBuf>> {
    use std::os::unix::ffi::OsStrExt;
    // getfsstat writes into caller-owned storage; no shared libc mount buffer.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 { return Err(io::Error::last_os_error()); }
    let mut mounts = Vec::<libc::statfs>::with_capacity(count as usize + 8);
    let count = unsafe { libc::getfsstat(mounts.as_mut_ptr(),
        (mounts.capacity() * std::mem::size_of::<libc::statfs>()) as _, libc::MNT_NOWAIT) };
    if count < 0 { return Err(io::Error::last_os_error()); }
    // Only the entries written by getfsstat may be read.
    unsafe { mounts.set_len((count as usize).min(mounts.capacity())); }
    Ok(mounts.iter().map(|mount| {
        let name = unsafe { std::ffi::CStr::from_ptr(mount.f_mntonname.as_ptr()) };
        PathBuf::from(std::ffi::OsStr::from_bytes(name.to_bytes()))
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_roots_are_unique_absolute_locations() {
        let roots = list().unwrap();
        assert!(!roots.is_empty());
        let mut paths = std::collections::HashSet::new();
        for (name, path) in roots {
            assert!(!name.is_empty() && !name.chars().any(char::is_control));
            assert!(path.is_absolute());
            assert!(paths.insert(path));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_table_preserves_escaped_and_non_utf8_names_without_directory_name_filters() {
        use std::os::unix::ffi::OsStrExt;
        let table = b"1 0 8:1 / / rw - ext4 /dev/sda rw\n\
            2 1 8:2 / /media/Drive\\040One rw - ext4 /dev/sdb rw\n\
            3 1 0:5 / /build rw - nfs server:/build rw\n\
            4 1 8:3 / /raw-\xff\\134name rw - ext4 /dev/sdc rw\n\
            malformed\n5 1 8:4 / /bad\\999 rw - ext4 /dev/sdd rw\n";
        let roots = linux_mounts(table);
        let bytes: Vec<_> = roots.iter().map(|path| path.as_os_str().as_bytes()).collect();
        assert_eq!(bytes, [b"/".as_slice(), b"/media/Drive One", b"/build", b"/raw-\xff\\name"]);
    }
}
