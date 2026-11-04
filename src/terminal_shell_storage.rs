//! Shared shell startup files stay with the executable's portable state.
use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};

pub(super) fn directory() -> PathBuf {
    crate::portable::data_dir()
        .join("shell-integration")
        .join(env!("CARGO_PKG_VERSION"))
}

pub(super) fn install_in(directory: &Path, name: &str, contents: &str) -> io::Result<PathBuf> {
    // Windows can deny competing replacements even when every staged file is
    // complete. Recheck under one lock so other shell starts reuse the winner.
    static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _install_guard = INSTALL_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    fs::create_dir_all(directory)?;
    let path = directory.join(name);
    match fs::read(&path) {
        Ok(existing) if existing == contents.as_bytes() => return Ok(path),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Publish a complete file even when several terminals launch together.
    // Staging also stays under .vtcode, never in the system temp directory.
    let mut staged = tempfile::Builder::new()
        .prefix(".install-")
        .tempfile_in(directory)?;
    staged.write_all(contents.as_bytes())?;
    if let Err(error) = staged.persist(&path) {
        // Another VTCode process may have published the same version meanwhile.
        if fs::read(&path).is_ok_and(|existing| existing == contents.as_bytes()) {
            return Ok(path);
        }
        return Err(error.error);
    }
    Ok(path)
}
