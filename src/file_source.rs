//! Handle-based source validation shared by editor, custom editors and ACP.
//! A path metadata check alone races replacements, and a normal FIFO open can
//! block before metadata is available. Never consume a nonregular file.
use std::{fs, io, path::Path};

pub fn open_document_source(
    path: &Path,
    allow_directory: bool,
) -> io::Result<(fs::File, fs::Metadata)> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    if allow_directory {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() && !(allow_directory && metadata.is_dir()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose a regular file",
        ));
    }
    Ok((file, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

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
