use std::ffi::{CString, c_char, c_int, c_void};

// Exercise the optional integer lookup used internally by rename detection.
// The safe git2 API uses a different getter that propagates config errors.
unsafe extern "C" {
    fn git_config_open_ondisk(out: *mut *mut c_void, path: *const c_char) -> c_int;
    fn git_config__get_int_force(config: *const c_void, key: *const c_char, fallback: c_int) -> c_int;
    fn git_config_free(config: *mut c_void);
}

struct Config(*mut c_void);

impl Drop for Config {
    fn drop(&mut self) {
        // SAFETY: open_ondisk returned this owned configuration on success.
        unsafe { git_config_free(self.0) }
    }
}

#[test]
fn config_read_error_preserves_forced_integer_fallback_and_recovers() {
    // Keep libgit2 initialized for the entire lifetime of the raw config.
    let _runtime = git2::Config::new().unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config");
    std::fs::write(&path, "[diff]\nrenameLimit = 7\n").unwrap();
    let name = CString::new(path.to_str().unwrap()).unwrap();
    let mut raw = std::ptr::null_mut();
    // SAFETY: the path is NUL-terminated and raw is a valid output pointer.
    assert_eq!(unsafe { git_config_open_ondisk(&mut raw, name.as_ptr()) }, 0);
    let config = Config(raw);
    let value = || {
        // SAFETY: config remains alive and the key is NUL-terminated.
        unsafe { git_config__get_int_force(config.0, c"diff.renamelimit".as_ptr(), 1000) }
    };

    assert_eq!(value(), 7);
    std::fs::write(&path, "[broken\n").unwrap();
    assert_eq!(value(), 1000);
    std::fs::write(&path, "[diff]\nrenameLimit = 21\n").unwrap();
    assert_eq!(value(), 21);
}
