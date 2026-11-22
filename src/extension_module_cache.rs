//! Native code for extension modules, kept across launches.
//!
//! Cranelift compiled every installed extension on each launch: a few MB of
//! Wasm cost seconds of CPU at startup before an extension could activate.
//! The compiled artifact is stored under the user's data directory, keyed by
//! the Wasm bytes and the engine's compatibility hash, so a later launch only
//! maps it back in. The cache never lives in a workspace: a cached artifact is
//! native code, and a repository must not be able to supply one.
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use wasmtime::{Engine, Module};

/// Artifacts kept; older ones belong to replaced extension builds.
const MAX_ENTRIES: usize = 32;

#[cfg(not(test))]
pub(crate) fn load(engine: &Engine, wasm: &Path) -> wasmtime::Result<Module> {
    load_in(&crate::portable::data_dir().join("wasm-cache"), engine, wasm)
}

fn load_in(dir: &Path, engine: &Engine, wasm: &Path) -> wasmtime::Result<Module> {
    let bytes = std::fs::read(wasm)?;
    let path = dir.join(entry_name(engine, &bytes));
    // SAFETY: only this module writes the cache directory, which is the
    // user's own data directory, and each file is renamed into place after
    // it was fully written. Wasmtime still rejects an artifact built by an
    // incompatible engine or configuration.
    if let Ok(module) = unsafe { Module::deserialize_file(engine, &path) } {
        return Ok(module);
    }
    let compiled = engine.precompile_module(&bytes)?;
    store(dir, &path, &compiled);
    // SAFETY: `compiled` was just produced by this engine.
    unsafe { Module::deserialize(engine, &compiled) }
}

fn entry_name(engine: &Engine, bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut engine_hash = std::hash::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut engine_hash);
    let digest = Sha256::new()
        .chain_update(engine_hash.finish().to_le_bytes())
        .chain_update(bytes)
        .finalize();
    let mut name = String::with_capacity(70);
    for byte in &digest[..20] {
        use std::fmt::Write;
        let _ = write!(name, "{byte:02x}"); // Error OK: writing to a String cannot fail
    }
    name.push_str(".cwasm");
    name
}

/// Best effort: a failed write only means the next launch compiles again.
fn store(dir: &Path, path: &Path, compiled: &[u8]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let partial = path.with_extension(format!("{}.partial", std::process::id()));
    if std::fs::write(&partial, compiled).is_err() || std::fs::rename(&partial, path).is_err() {
        let _ = std::fs::remove_file(&partial); // Error OK: best-effort cleanup
        return;
    }
    prune(dir, path);
}

fn prune(dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path != keep && path.extension().is_some_and(|ext| ext == "cwasm"))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    if entries.len() < MAX_ENTRIES {
        return;
    }
    entries.sort();
    for (_, path) in &entries[..entries.len() + 1 - MAX_ENTRIES] {
        let _ = std::fs::remove_file(path); // Error OK: another window may have removed it
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAT: &str = r#"(module (func (export "vtcode_activate") (result i32) i32.const 0))"#;

    #[test]
    fn compiled_modules_are_reused_and_follow_the_wasm_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let wasm = dir.path().join("ext.wasm");
        std::fs::write(&wasm, WAT).unwrap();
        let engine = crate::extensions::new_extension_engine();

        let first = load_in(&cache, &engine, &wasm).unwrap();
        assert!(first.get_export("vtcode_activate").is_some());
        let cached: Vec<_> = std::fs::read_dir(&cache).unwrap().flatten().collect();
        assert_eq!(cached.len(), 1, "one artifact, no partial file left");
        let artifact = cached[0].path();
        let written = std::fs::metadata(&artifact).unwrap().modified().unwrap();

        load_in(&cache, &engine, &wasm).unwrap();
        assert_eq!(
            std::fs::metadata(&artifact).unwrap().modified().unwrap(),
            written,
            "a cached artifact is loaded, not rewritten"
        );

        // A damaged artifact is replaced instead of failing activation.
        std::fs::write(&artifact, b"not a module").unwrap();
        assert!(load_in(&cache, &engine, &wasm).is_ok());
        assert!(unsafe { Module::deserialize_file(&engine, &artifact) }.is_ok());

        // A changed extension build gets its own artifact.
        let changed = WAT.replace("i32.const 0", "i32.const 1");
        std::fs::write(&wasm, changed).unwrap();
        load_in(&cache, &engine, &wasm).unwrap();
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 2);
    }

    #[test]
    fn invalid_wasm_still_fails() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("bad.wasm");
        std::fs::write(&wasm, b"\0asm garbage").unwrap();
        let engine = crate::extensions::new_extension_engine();
        assert!(load_in(&dir.path().join("cache"), &engine, &wasm).is_err());
    }
}
