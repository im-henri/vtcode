# VTCode libgit2 patch

This directory vendors libgit2-sys 0.18.3+1.9.2, including libgit2 1.9.2.
The original Rust and libgit2 license files are preserved.

In `libgit2/src/libgit2/config.c`, `get_entry` now publishes the backend
entry only after a successful read. A backend error can leave its output
uninitialized. Publishing that output previously let the optional integer
configuration getters read or free an invalid entry after suppressing the
error. This caused a native crash during background Git status refreshes.

`src/git_config_error_tests.rs` exercises the internal integer getter used
by rename detection: read a valid value, encounter a malformed configuration,
return the supplied fallback, then recover after the configuration is fixed.
Without this patch the test returned a stale value and corrupted the heap.

VTCode enables `git2`'s `vendored-libgit2` feature so release builds always
include this fix instead of selecting an unpatched system library.
