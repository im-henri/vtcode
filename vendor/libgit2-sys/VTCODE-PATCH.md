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

`vtcode_git_hash_blob_stream` uses libgit2's collision-detecting Git blob
hash on bytes supplied by a caller. Rollback can hash an already verified
file handle and reuse existing objects without opening the source by name.
Object write streams also propagate hash update and finalization errors.

Stash save retains the original index timestamp when diffing a copy of its
entries. Otherwise the copied stat cache can hide edits that share the index's
timestamp, and stash can reset those edits without saving them. Index load and
copy failures also preserve their native error codes.
