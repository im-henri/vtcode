use std::fs;
use std::path::{Path, PathBuf};

use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use crate::textmate::{SyntaxRegistry, SyntaxRegistryBuilder};
use walkdir::WalkDir;

const CACHE_FORMAT: &str = "vtcode-textmate-registry-v1-irosashi-0.3.0";
const CACHE_DIRECTORY: &str = ".cache";
const CACHE_FILE: &str = "textmate-registry.json.gz";
const FINGERPRINT_FILE: &str = "source.fingerprint";

#[cfg(test)]
thread_local! { static CACHE_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

fn read_syntax_cache(path: &Path) -> Option<SyntaxRegistry> {
    #[cfg(test)]
    CACHE_LOADS.set(CACHE_LOADS.get() + 1);
    let file = fs::File::open(path).ok()?;
    let sources: std::collections::BTreeMap<String,crate::textmate::GrammarSource> = serde_json::from_reader(GzDecoder::new(file)).ok()?;
    let mut builder=SyntaxRegistryBuilder::new();
    for source in sources.into_values() { builder.add(source); }
    builder.build().ok()
}

pub(crate) struct StartupSyntaxRegistry {
    pub(crate) syntax_set: SyntaxRegistry,
    /// None means startup used the embedded TextMate registry. The background
    /// refresh must then install even a cache another process created between
    /// the startup read and the freshness check.
    pub(crate) fingerprint: Option<u64>,
}

pub(crate) struct RefreshedSyntaxRegistry {
    pub(crate) syntax_set: SyntaxRegistry,
    pub(crate) fingerprint: Option<u64>,
    pub(crate) cache_write_error: Option<String>,
}

pub(crate) fn build_syntax_set(folders: &[PathBuf]) -> Result<SyntaxRegistry,String> {
    let mut builder=SyntaxRegistryBuilder::new();
    for folder in folders {add_syntax_folder(&mut builder,folder)?;}
    builder.build()
}

fn override_entries(folder: &Path) -> impl Iterator<Item = Result<walkdir::DirEntry, walkdir::Error>> {
    let bundled = folder.join("bundled");
    let compiled_catalog = folder.join("BUNDLED-GRAMMARS.json").is_file()
        && bundled.join("index.json").is_file();
    // The base catalog is compiled into the engine. Parsing it as overrides
    // would replace the curated VS Code packages and duplicate the cache.
    WalkDir::new(folder).follow_links(false).into_iter().filter_entry(move |entry| {
        entry.file_name() != ".cache" && (!compiled_catalog || entry.path() != bundled)
    })
}

pub(crate) fn add_syntax_folder(builder: &mut SyntaxRegistryBuilder, folder: &Path) -> Result<(),String> {
    if !folder.exists() {return Ok(());}
    let mut files=Vec::new();
    for entry in override_entries(folder) {
        let entry=entry.map_err(|e|e.to_string())?;
        if entry.file_type().is_file() && crate::textmate_grammar::is_grammar_path(entry.path()) {files.push(entry.into_path());}
    }
    files.sort();
    let mut contributed=std::collections::BTreeSet::new();
    for manifest in files.iter().filter(|p|p.file_name().is_some_and(|n|n=="package.json")) {
        contributed.extend(crate::textmate_grammar::add_package(builder,manifest)?);
    }
    for path in files {
        if contributed.contains(&path) || path.file_name().is_some_and(|n|n=="package.json") {continue;}
        let bytes=fs::read(&path).map_err(|e|format!("{}: {e}",path.display()))?;
        let value=crate::textmate_grammar::parse_value(&bytes).map_err(|e|format!("{}: {e}",path.display()))?;
        // A folder may also contain manifests and provenance JSON.
        if value.get("scopeName").is_some() {builder.add(crate::textmate_grammar::parse_source(&bytes,path.file_stem().and_then(|s|s.to_str()).unwrap_or("TextMate"))?);}
        else if path.file_name().is_some_and(|n|n.to_string_lossy().ends_with(".tmLanguage.json")) || path.extension().is_some_and(|e|e.eq_ignore_ascii_case("tmLanguage")) {
            return Err(format!("{}: TextMate grammar needs a scopeName",path.display()));
        }
    }
    Ok(())
}

pub(crate) fn installed_syntax_folder() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok();
    let folder = resolve_installed_syntax_folder(executable.as_deref());
    // Test binaries can live in an external CARGO_TARGET_DIR. Only tests may
    // fall back to the checkout; installed binaries still require their assets.
    #[cfg(test)]
    let folder = folder.or_else(|| {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("grammars");
        source.is_dir().then_some(source)
    });
    folder
}

fn resolve_installed_syntax_folder(executable: Option<&Path>) -> Option<PathBuf> {
    // Installed distributions keep editable product assets beside the binary.
    let beside_executable = executable
        .and_then(Path::parent)
        .map(|parent| parent.join("grammars"));
    if beside_executable.as_ref().is_some_and(|path| path.is_dir()) {
        return beside_executable;
    }

    // Discover development assets relative to the running binary. Do not embed
    // the build machine's checkout path in a distributed executable.
    let target_dir = executable?.ancestors().find(|path| {
        path.file_name().is_some_and(|name| name == "target")
    })?;
    let source_tree = target_dir.parent()?.join("grammars");
    source_tree.is_dir().then_some(source_tree)
}

#[cfg(test)]
pub(crate) fn load_installed_syntax_set(folder: Option<&Path>) -> Result<SyntaxRegistry, String> {
    refresh_installed_syntax_set(folder, true, None)?
        .map(|refreshed| refreshed.syntax_set)
        .ok_or_else(|| {
            "installed syntax refresh unexpectedly produced no startup syntax set".to_string()
        })
}

/// Load only the registry snapshot on the startup asset worker. Source
/// freshness requires reading every grammar and is deliberately handled by a
/// one-shot background refresh after the UI is visible.
pub(crate) fn load_startup_syntax_set(
    folder: Option<&Path>,
) -> Result<StartupSyntaxRegistry, String> {
    let Some(folder) = folder else {
        return Ok(StartupSyntaxRegistry {
            syntax_set: build_syntax_set(&[])?,
            fingerprint: None,
        });
    };
    let fingerprint_path = folder.join(CACHE_DIRECTORY).join(FINGERPRINT_FILE);
    let cache_path = folder.join(CACHE_DIRECTORY).join(CACHE_FILE);
    let fingerprint = fs::read_to_string(fingerprint_path).ok().and_then(|saved| {
        saved.strip_prefix(&format!("{CACHE_FORMAT}:"))
            .and_then(|hash| u64::from_str_radix(hash.trim(), 16).ok())
    });
    if fingerprint.is_some() {
        if let Some(syntax_set) = read_syntax_cache(&cache_path) {
            return Ok(StartupSyntaxRegistry {
                syntax_set,
                fingerprint,
            });
        }
    }

    Ok(StartupSyntaxRegistry {
        syntax_set: build_syntax_set(&[])?,
        fingerprint: None,
    })
}

/// Validate source grammars and refresh their registry snapshot. This reads
/// all source files and must run on a worker, never on the UI thread.
/// `force_install` is used when startup had to show the embedded TextMate registry.
/// Compare against the registry actually loaded by this process, since another
/// instance may already have rebuilt the on-disk cache from newer sources.
pub(crate) fn refresh_installed_syntax_set(
    folder: Option<&Path>,
    force_install: bool,
    loaded_fingerprint: Option<u64>,
) -> Result<Option<RefreshedSyntaxRegistry>, String> {
    let Some(folder) = folder else {
        return if force_install {
            build_syntax_set(&[]).map(|syntax_set| {
                Some(RefreshedSyntaxRegistry {
                    syntax_set,
                    fingerprint: None,
                    cache_write_error: None,
                })
            })
        } else {
            Ok(None)
        };
    };
    let fingerprint = syntax_source_fingerprint(folder)?;
    // The live registry already represents these exact source bytes. Do not
    // deserialize (and immediately discard) the entire registry snapshot again. This
    // also works when a previous cache write failed: the in-memory set is valid.
    if !force_install && loaded_fingerprint == Some(fingerprint) {
        return Ok(None);
    }
    let expected = fingerprint_text(fingerprint);
    let cache_directory = folder.join(CACHE_DIRECTORY);
    let cache_path = cache_directory.join(CACHE_FILE);
    let fingerprint_path = cache_directory.join(FINGERPRINT_FILE);

    if fs::read_to_string(&fingerprint_path).is_ok_and(|saved| saved == expected) {
        if let Some(syntax_set) = read_syntax_cache(&cache_path) {
            return Ok((force_install || loaded_fingerprint != Some(fingerprint)).then_some(
                RefreshedSyntaxRegistry {
                    syntax_set,
                    fingerprint: Some(fingerprint),
                    cache_write_error: None,
                },
            ));
        }
    }

    let syntax_set = build_syntax_set(&[folder.to_path_buf()])?;
    let cache_write_error = write_syntax_cache(folder, &syntax_set, fingerprint)
        .err()
        .map(|error| {
            format!(
                "could not refresh syntax cache in {}: {error}",
                cache_directory.display()
            )
        });
    Ok(Some(RefreshedSyntaxRegistry {
        syntax_set,
        fingerprint: Some(fingerprint),
        cache_write_error,
    }))
}

#[allow(dead_code)] // Invoked by examples/rebuild_syntax_cache.rs for release packaging.
pub(crate) fn rebuild_syntax_cache(folder: &Path) -> Result<(usize, u64), String> {
    let fingerprint = syntax_source_fingerprint(folder)?;
    let syntax_set = build_syntax_set(&[folder.to_path_buf()])?;
    let syntax_count = syntax_set.syntaxes().len();
    write_syntax_cache(folder, &syntax_set, fingerprint)?;
    Ok((syntax_count, fingerprint))
}

fn write_syntax_cache(
    folder: &Path,
    syntax_set: &SyntaxRegistry,
    fingerprint: u64,
) -> Result<(), String> {
    let cache_directory = folder.join(CACHE_DIRECTORY);
    fs::create_dir_all(&cache_directory)
        .map_err(|error| format!("{}: {error}", cache_directory.display()))?;
    let cache_path = cache_directory.join(CACHE_FILE);
    let cache_temp = cache_directory.join(format!(".{CACHE_FILE}.{}.tmp", std::process::id()));
    let file=fs::File::create(&cache_temp).map_err(|e|e.to_string())?;
    let mut encoder=GzEncoder::new(file,Compression::fast());
    serde_json::to_writer(&mut encoder,&syntax_set.sources).map_err(|e|e.to_string())?;
    encoder.finish().map_err(|e|e.to_string())?;
    replace_cache_file(&cache_temp, &cache_path)?;
    let fingerprint_path = cache_directory.join(FINGERPRINT_FILE);
    let fingerprint_temp =
        cache_directory.join(format!(".{FINGERPRINT_FILE}.{}.tmp", std::process::id()));
    fs::write(&fingerprint_temp, fingerprint_text(fingerprint))
        .map_err(|error| format!("{}: {error}", fingerprint_temp.display()))?;
    replace_cache_file(&fingerprint_temp, &fingerprint_path)
}

fn replace_cache_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    if let Err(first_error) = fs::rename(temporary, destination) {
        // Windows does not replace an existing destination with `rename`.
        // The complete new artifact is already on disk, so only the final
        // handoff has a tiny gap rather than exposing a partially written dump.
        if destination.is_file() {
            fs::remove_file(destination)
                .map_err(|error| format!("{}: {error}", destination.display()))?;
            if let Err(error) = fs::rename(temporary, destination) {
                let _ = fs::remove_file(temporary);
                return Err(format!("{}: {error}", destination.display()));
            }
        } else {
            let _ = fs::remove_file(temporary);
            return Err(format!("{}: {first_error}", destination.display()));
        }
    }
    Ok(())
}

fn fingerprint_text(fingerprint: u64) -> String {
    format!("{CACHE_FORMAT}:{fingerprint:016x}\n")
}

fn syntax_source_fingerprint(folder: &Path) -> Result<u64, String> {
    let mut files = Vec::new();
    for entry in override_entries(folder) {
        let entry = entry.map_err(|error| format!("{}: {error}", folder.display()))?;
        if entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tmLanguage") || extension.eq_ignore_ascii_case("plist") || extension.eq_ignore_ascii_case("json"))
        {
            files.push(entry.path().to_path_buf());
        }
    }
    files.sort_by_cached_key(|path| normalized_relative_path(folder, path));

    let mut hash = 0xcbf29ce484222325u64;
    for path in files {
        let relative = normalized_relative_path(folder, &path);
        update_hash(&mut hash, relative.as_bytes());
        update_hash(&mut hash, &[0]);
        let bytes = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        update_hash(&mut hash, &bytes);
        update_hash(&mut hash, &[0xff]);
    }
    Ok(hash)
}

fn normalized_relative_path(folder: &Path, path: &Path) -> String {
    path.strip_prefix(folder)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn update_hash(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_live_registry_does_not_deserialize_the_cache_again() {
        let directory = tempfile::tempdir().unwrap();
        let syntax = directory.path().join("Example.tmLanguage.json");
        write_test_syntax(&syntax, "First Language", "first");
        rebuild_syntax_cache(directory.path()).unwrap();
        let startup = load_startup_syntax_set(Some(directory.path())).unwrap();
        CACHE_LOADS.set(0);
        assert!(refresh_installed_syntax_set(Some(directory.path()), false, startup.fingerprint).unwrap().is_none());
        assert_eq!(CACHE_LOADS.get(), 0);
        // A different in-memory generation must still read a valid newer cache.
        assert!(refresh_installed_syntax_set(Some(directory.path()), false, Some(0)).unwrap().is_some());
        assert_eq!(CACHE_LOADS.get(), 1);
        write_test_syntax(&syntax, "Changed Language", "changed");
        assert!(refresh_installed_syntax_set(Some(directory.path()), false, startup.fingerprint).unwrap().is_some());
    }

    #[test]
    #[ignore = "manual optimized startup component benchmark"]
    fn benchmark_startup_syntax_work() {
        use std::{hint::black_box, time::Instant};
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("grammars");
        let loaded = refresh_installed_syntax_set(Some(&folder), true, None).unwrap().unwrap();
        let cache = folder.join(CACHE_DIRECTORY).join(CACHE_FILE);
        let mut before = Vec::new();
        let mut after = Vec::new();
        let mut load = Vec::new();
        for _ in 0..9 {
            let start = Instant::now();
            black_box(syntax_source_fingerprint(&folder).unwrap());
            black_box(read_syntax_cache(&cache).unwrap());
            before.push(start.elapsed());
            let start = Instant::now();
            assert!(refresh_installed_syntax_set(Some(&folder), false, loaded.fingerprint).unwrap().is_none());
            after.push(start.elapsed());
            let start = Instant::now();
            black_box(load_startup_syntax_set(Some(&folder)).unwrap());
            load.push(start.elapsed());
        }
        before.sort(); after.sort(); load.sort();
        eprintln!("startup syntax: synchronous cache load moved off UI = {:?}; unchanged validation before = {:?}, after = {:?}", load[4], before[4], after[4]);
    }

    fn write_test_syntax(path: &Path, name: &str, extension: &str) {
        fs::write(path,serde_json::json!({"name":name,"scopeName":"source.test","fileTypes":[extension],
            "patterns":[{"match":"\\bmagic\\b","name":"keyword.control.test"}]}).to_string()).unwrap();
    }

    #[test]
    fn source_fingerprint_changes_with_grammar_content() {
        let directory = tempfile::tempdir().unwrap();
        let syntax = directory.path().join("Example.tmLanguage.json");
        fs::write(&syntax, "name: One").unwrap();
        let before = syntax_source_fingerprint(directory.path()).unwrap();
        fs::write(&syntax, "name: Two").unwrap();
        let after = syntax_source_fingerprint(directory.path()).unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn native_grammar_and_manifest_edits_invalidate_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let initial = syntax_source_fingerprint(directory.path()).unwrap();
        let grammar = directory.path().join("Example.tmLanguage.json");
        fs::write(&grammar, "{}").unwrap();
        let added = syntax_source_fingerprint(directory.path()).unwrap();
        assert_ne!(initial, added);
        fs::write(&grammar, "{\"name\":\"Changed\"}").unwrap();
        let edited = syntax_source_fingerprint(directory.path()).unwrap();
        assert_ne!(added, edited);
        fs::write(directory.path().join("package.json"), "{}").unwrap();
        assert_ne!(edited, syntax_source_fingerprint(directory.path()).unwrap());
    }

    #[test]
    fn source_edits_and_removals_invalidate_the_disk_cache() {
        let directory = tempfile::tempdir().unwrap();
        let syntax = directory.path().join("Example.tmLanguage.json");
        write_test_syntax(&syntax, "First Language", "first");

        let first = load_installed_syntax_set(Some(directory.path())).unwrap();
        assert_eq!(
            first.find_syntax_by_extension("first").unwrap().name,
            "First Language"
        );
        assert!(directory
            .path()
            .join(CACHE_DIRECTORY)
            .join(CACHE_FILE)
            .is_file());

        write_test_syntax(&syntax, "Second Language", "second");
        let edited = load_installed_syntax_set(Some(directory.path())).unwrap();
        assert!(edited.find_syntax_by_extension("first").is_none());
        assert_eq!(
            edited.find_syntax_by_extension("second").unwrap().name,
            "Second Language"
        );

        fs::remove_file(syntax).unwrap();
        let removed = load_installed_syntax_set(Some(directory.path())).unwrap();
        assert!(removed.find_syntax_by_extension("second").is_none());
        assert!(removed.find_syntax_by_name("Rust").is_some());
        assert_eq!(removed.find_syntax_plain_text().name, "Plain Text");
    }

    #[test]
    fn startup_uses_cache_immediately_then_background_refreshes_edits() {
        let directory = tempfile::tempdir().unwrap();
        let syntax = directory.path().join("Example.tmLanguage.json");
        write_test_syntax(&syntax, "First Language", "first");
        rebuild_syntax_cache(directory.path()).unwrap();

        let startup = load_startup_syntax_set(Some(directory.path())).unwrap();
        assert!(startup.fingerprint.is_some());
        assert!(startup.syntax_set.find_syntax_by_extension("first").is_some());

        write_test_syntax(&syntax, "Second Language", "second");
        let stale_startup = load_startup_syntax_set(Some(directory.path())).unwrap();
        assert!(stale_startup.fingerprint.is_some());
        assert!(stale_startup
            .syntax_set
            .find_syntax_by_extension("first")
            .is_some());

        let refreshed = refresh_installed_syntax_set(Some(directory.path()), false, stale_startup.fingerprint)
            .unwrap()
            .expect("edited source must refresh automatically");
        assert!(refreshed
            .syntax_set
            .find_syntax_by_extension("first")
            .is_none());
        assert_eq!(
            refreshed
                .syntax_set
                .find_syntax_by_extension("second")
                .unwrap()
                .name,
            "Second Language"
        );

        let next_startup = load_startup_syntax_set(Some(directory.path())).unwrap();
        assert_eq!(
            next_startup
                .syntax_set
                .find_syntax_by_extension("second")
                .unwrap()
                .name,
            "Second Language"
        );
        assert!(
            refresh_installed_syntax_set(Some(directory.path()), false, next_startup.fingerprint)
                .unwrap()
                .is_none(),
            "a current cache must not rebuild or republish the registry"
        );
    }

    #[test]
    fn missing_cache_is_built_automatically_after_fast_plain_startup() {
        let directory = tempfile::tempdir().unwrap();
        write_test_syntax(
            &directory.path().join("Example.tmLanguage.json"),
            "Example Language",
            "example",
        );

        let startup = load_startup_syntax_set(Some(directory.path())).unwrap();
        assert!(startup.fingerprint.is_none());
        assert!(startup.syntax_set.find_syntax_by_name("Rust").is_some());

        let refreshed = refresh_installed_syntax_set(Some(directory.path()), true, startup.fingerprint)
            .unwrap()
            .expect("missing cache must be created automatically");
        assert!(refreshed
            .syntax_set
            .find_syntax_by_extension("example")
            .is_some());
        assert!(directory
            .path()
            .join(CACHE_DIRECTORY)
            .join(CACHE_FILE)
            .is_file());
        assert!(directory
            .path()
            .join(CACHE_DIRECTORY)
            .join(FINGERPRINT_FILE)
            .is_file());
    }

    #[test]
    fn repository_ships_the_complete_textmate_source_set() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("grammars");
        let bundled = folder.join("bundled");
        let index: serde_json::Value = serde_json::from_slice(&fs::read(bundled.join("index.json")).unwrap()).unwrap();
        assert_eq!(index["grammars"].as_object().unwrap().len(), 257);
        let provenance: serde_json::Value = serde_json::from_slice(&fs::read(folder.join("BUNDLED-GRAMMARS.json")).unwrap()).unwrap();
        use sha2::Digest;
        for (name, expected) in provenance["grammars"].as_object().unwrap() {
            let bytes = fs::read(bundled.join(name)).unwrap();
            assert_eq!(format!("sha256:{:x}", sha2::Sha256::digest(bytes)), expected.as_str().unwrap(), "{name}");
        }
        assert!(!Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/irosashi/assets/grammars").exists());
        assert_eq!(WalkDir::new(&folder).into_iter().filter_map(Result::ok)
            .filter(|e|e.file_type().is_file() && e.path().extension().is_some_and(|s|s=="sublime-syntax")).count(),0);
        let syntax_set=load_installed_syntax_set(Some(&folder)).unwrap();
        assert_eq!(syntax_set.sources.len(), 6, "the runtime cache contains the overrides, not a second copy of the bundled catalog");
        assert!(syntax_set.syntaxes().len()>=258);
        assert!(syntax_set.find_syntax_by_name("Rust").is_some());
        assert!(syntax_set.find_syntax_by_name("PowerShell").is_some());
        assert!(syntax_set.find_syntax_by_name("TypeScript").is_some());
        assert!(syntax_set.find_syntax_by_name("TSX").is_some());
        assert!(syntax_set.find_syntax_by_name("TOML").is_some());
        assert!(syntax_set.find_syntax_by_name("Dockerfile").is_some());
        assert!(syntax_set.find_syntax_by_name("CMake").is_some());
        assert!(syntax_set.find_syntax_by_name("INI").is_some());
        assert!(syntax_set.find_syntax_by_name("Terraform").is_some());
        assert!(syntax_set.find_syntax_by_name("GraphQL").is_some());
        assert!(syntax_set.find_syntax_by_name("Protocol Buffer").is_some());
        assert!(syntax_set.find_syntax_by_name("VHDL").is_some());
        assert!(syntax_set.find_syntax_by_name("Verilog").is_some());
        assert!(syntax_set.find_syntax_by_name("SystemVerilog").is_some());
        assert!(syntax_set.find_syntax_by_name("Plain Text").is_some());
    }

    #[test]
    fn source_tree_assets_are_available_to_every_cargo_profile() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for profile in [
            "release/vtcode",
            "debug/deps/vtcode-tests",
            "x86_64-unknown-linux-gnu/release/vtcode",
        ] {
            let executable = manifest.join("target").join(profile);
            assert_eq!(
                resolve_installed_syntax_folder(Some(&executable)),
                Some(manifest.join("grammars"))
            );
        }
    }

    #[test]
    fn installed_assets_take_precedence_over_source_tree_assets() {
        let directory = tempfile::tempdir().unwrap();
        let install = directory.path().join("install");
        let source = directory.path().join("source");
        fs::create_dir_all(install.join("grammars")).unwrap();
        fs::create_dir_all(source.join("grammars")).unwrap();

        let executable = install.join("vtcode");
        assert_eq!(
            resolve_installed_syntax_folder(Some(&executable)),
            Some(install.join("grammars"))
        );
    }

    #[test]
    fn relocated_binary_without_assets_does_not_use_the_source_tree() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        fs::create_dir_all(source.join("grammars")).unwrap();
        let executable = directory.path().join("portable/vtcode");
        assert_eq!(resolve_installed_syntax_folder(Some(&executable)), None);
        assert_eq!(resolve_installed_syntax_folder(None), None);
    }
}
