use super::*;
use std::collections::BTreeMap;

// Original UTF-16 evaluation remains an independent matching oracle. Expected
// results never use the ASCII matcher, including in mixed-pattern groups.
fn legacy_match(globs: &SearchGlobs, path: &Path) -> bool {
    let text = path
        .to_string_lossy()
        .replace('\\', "/")
        .encode_utf16()
        .collect::<Vec<_>>();
    globs
        .0
        .iter()
        .any(|regex| regex.find_from_ucs2(&text, 0).next().is_some())
}

fn legacy_includes(paths: &SearchPaths, path: &Path) -> bool {
    paths.folders.iter().any(|(root, pattern)| {
        crate::native_paths::strip_prefix(path, root).is_some_and(|relative| {
            (!paths.has_global && pattern.is_none())
                || legacy_match(&paths.global, relative)
                || pattern
                    .as_ref()
                    .is_some_and(|glob| legacy_match(glob, relative))
        })
    })
}

fn legacy_includes_open(paths: &SearchPaths, path: &Path) -> bool {
    (!paths.has_global && !paths.using_paths)
        || legacy_match(&paths.global, path)
        || paths.using_paths && legacy_includes(paths, path)
}

fn legacy_excludes(paths: &SearchPaths, path: &Path, workspace: &Path) -> bool {
    paths.has_global
        && legacy_match(
            &paths.global,
            crate::native_paths::strip_prefix(path, workspace).unwrap_or(path),
        )
        || paths.using_paths
            && paths.folders.iter().any(|(root, pattern)| {
                crate::native_paths::strip_prefix(path, root).is_some_and(|relative| {
                    pattern
                        .as_ref()
                        .is_none_or(|glob| legacy_match(glob, relative))
                })
            })
}

fn legacy_settings_exclude(rules: &ExcludeRules, path: &Path, root: &Path) -> bool {
    let relative = crate::native_paths::strip_prefix(path, root).unwrap_or(path);
    rules.0.iter().any(|(glob, when)| {
        glob.0.iter().any(|regex| {
            let text = relative
                .to_string_lossy()
                .replace('\\', "/")
                .encode_utf16()
                .collect::<Vec<_>>();
            regex.find_from_ucs2(&text, 0).next().is_some()
        }) && when.as_ref().is_none_or(|when| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    path.with_file_name(when.replace("$(basename)", stem))
                        .exists()
                })
        })
    })
}

#[test]
fn ascii_glob_sets_preserve_the_original_engine_for_mixed_and_unusual_paths() {
    let mut names = vec![
        "".to_owned(),
        "src/main.rs".into(),
        "src//main.rs".into(),
        "src/line\nname.rs".into(),
        "src/line\rname.rs".into(),
        "src/main.rs\n".into(),
        "a.rs".into(),
        "&.rs".into(),
        "界/🦀.rs".into(),
        "src/é.rs".into(),
    ];
    let alphabet = [
        'a', 'b', 'A', '0', '_', '/', '\\', '.', '{', '}', '[', ']', '(', ')', '?', '+', '$', ' ',
        '\n', '\r', '\t', '\0', 'é', '😀',
    ];
    let mut state = 0xb4f0_0918u64;
    for _ in 0..2000 {
        let mut name = String::new();
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let len = (state % 60) as usize;
        for _ in 0..len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            name.push(alphabet[(state >> 32) as usize % alphabet.len()]);
        }
        names.push(name);
    }
    for source in [
        "**",
        "*",
        "./src/**/*.rs",
        "**/{*.rs,*.toml}",
        "./{src,lib}/**/???.rs",
        "*.cps, [a&&b].rs, Ω??.rs",
        "./src/**, ./lib/**, **/foo*.{rs,ts}",
        "./{**,src}/file",
        "./src/{,nested}/**",
        "./src/{broken",
        "./src/a$+()*.rs",
    ] {
        let globs = compile_patterns(source);
        assert!(
            globs.1.is_some(),
            "{source} should exercise the ASCII matcher"
        );
        for name in &names {
            let path = Path::new(name);
            assert_eq!(
                Candidate::new(path).matches(&globs, path),
                legacy_match(&globs, path),
                "{source:?}, {name:?}"
            );
        }
    }
}

#[test]
fn ascii_matching_avoids_utf16_allocation_until_a_fallback_is_needed() {
    let path = Path::new("src/main.rs");
    let candidate = Candidate::new(path);
    assert!(candidate.matches(&compile_patterns("*.rs, *.js"), path));
    assert!(!candidate.matches(&compile_patterns("*.ts"), path));
    assert!(
        candidate
            .prepared
            .borrow()
            .first
            .as_ref()
            .unwrap()
            .1
            .utf16
            .get()
            .is_none()
    );
    assert!(candidate.matches(&compile_patterns("**/m[--a]in.rs"), path));
    assert!(
        candidate
            .prepared
            .borrow()
            .first
            .as_ref()
            .unwrap()
            .1
            .utf16
            .get()
            .is_some()
    );
    assert_eq!(candidate.preparations.get(), 1);
}

#[test]
fn ascii_character_classes_match_javascript_including_set_operator_literals() {
    let mut classes = vec![
        "a-z".to_owned(),
        "!a-z".into(),
        "A-z".into(),
        "a&&b".into(),
        "a~~b".into(),
        "-a".into(),
        "a-".into(),
        "a-b-c".into(),
        "--a".into(),
        "a[]]".into(),
    ];
    let alphabet = [
        'a', 'b', 'z', 'A', '0', '-', '&', '~', '^', '!', ']', '[', '.',
    ];
    let mut state = 0xc3a5u64;
    for _ in 0..250 {
        let mut class = String::new();
        for _ in 0..4 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            class.push(alphabet[(state >> 32) as usize % alphabet.len()]);
        }
        classes.push(class);
    }
    for class in classes {
        let source = format!("./src/[{class}]*.rs, *.cps");
        let globs = compile_patterns(&source);
        for ch in 0..128u8 {
            let name = format!("src/{}file.rs", char::from(ch));
            let path = Path::new(&name);
            assert_eq!(
                Candidate::new(path).matches(&globs, path),
                legacy_match(&globs, path),
                "{source:?}, {name:?}"
            );
        }
    }
}

#[test]
fn compiled_path_plan_cache_is_bounded_and_separates_roots_and_filters() {
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let first = SearchPaths::cached(root.path(), "./src/**/*.rs");
    assert!(Arc::ptr_eq(
        &first,
        &SearchPaths::cached(root.path(), "./src/**/*.rs")
    ));
    assert!(!Arc::ptr_eq(
        &first,
        &SearchPaths::cached(&root.path().join("other"), "./src/**/*.rs")
    ));
    assert!(!Arc::ptr_eq(
        &first,
        &SearchPaths::cached(root.path(), "./lib/**/*.rs")
    ));
    for i in 0..5 {
        SearchPaths::cached(root.path(), &format!("./other_{i}/**"));
    }
    assert!(!Arc::ptr_eq(
        &first,
        &SearchPaths::cached(root.path(), "./src/**/*.rs")
    ));
    let large = ",".repeat(4097);
    assert!(!Arc::ptr_eq(
        &SearchPaths::cached(root.path(), &large),
        &SearchPaths::cached(root.path(), &large)
    ));
}

#[test]
fn prepared_globs_preserve_exact_separator_case_and_utf16_behavior() {
    for name in [
        "",
        "main.rs",
        "Main.RS",
        "src/main.rs",
        "src//main.rs",
        "./src/main.rs",
        "src/main.rs/",
        r"src\main.rs",
        r"src\\nested\main.rs",
        "src/café.rs",
        "界/🦀.rs",
        "src/e\u{301}.rs",
        "src/line\nname.rs",
        "a[1]/{b,c}.rs",
    ] {
        let path = Path::new(name);
        let candidate = Candidate::new(path);
        for pattern in [
            "",
            "  ,  ",
            ".",
            "*.rs",
            "**",
            "./src",
            "src/*.rs",
            "src/**",
            "?.rs",
            "??.rs",
            "???.rs",
            "**/{*.rs,*.toml}",
            "**/[a-z]*.rs",
            r"src\**\*.rs",
            "**/*[[]1[]]/*",
            "*.RS",
        ] {
            let globs = compile_patterns(pattern);
            assert_eq!(
                candidate.matches(&globs, path),
                legacy_match(&globs, path),
                "{name:?}, {pattern:?}"
            );
        }
        assert_eq!(candidate.preparations.get(), 1, "{name:?}");
    }
    assert!(!matches_glob("🦀.rs", "?.rs"));
    assert!(matches_glob("🦀.rs", "??.rs"));
    assert!(matches_glob("e\u{301}.rs", "??.rs"));
}

#[test]
fn repeated_cached_patterns_share_one_candidate_conversion() {
    let candidate = Candidate::new(Path::new(r"src\nested\café🦀.rs"));
    for _ in 0..3 {
        assert!(matches_glob_prepared(&candidate, "*.rs, *.toml"));
        assert!(!matches_glob_prepared(&candidate, "*.js"));
        assert!(!matches_glob_prepared(&candidate, "./generated"));
    }
    assert_eq!(candidate.preparations.get(), 1);
}

#[test]
fn include_exclude_and_setting_rules_share_the_relative_representation() {
    let workspace = std::env::current_dir().unwrap();
    let path = workspace.join("src/café🦀/main.rs");
    let candidate = Candidate::new(&path);
    let include = SearchPaths::new(&workspace, "./other, ./src, *.js");
    let exclude = SearchPaths::new(&workspace, "*.tmp, ./generated");
    let settings = ExcludeRules::new(&BTreeMap::new(), &BTreeMap::new());
    assert!(include.includes(&candidate));
    assert!(!exclude.excludes(&candidate, &workspace));
    assert!(!settings.excludes(&candidate, &workspace));
    assert_eq!(candidate.preparations.get(), 1);
    assert_eq!(
        candidate
            .prepared
            .borrow()
            .first
            .as_ref()
            .unwrap()
            .1
            .utf16(),
        "src/café🦀/main.rs".encode_utf16().collect::<Vec<_>>()
    );
}

#[test]
fn absolute_and_multiple_root_spellings_are_independent_and_reused() {
    let workspace = std::env::current_dir().unwrap();
    let path = workspace.join("src/deep/🦀.rs");
    let nested = workspace.join("src");
    let candidate = Candidate::new(&path);
    let workspace_relative = crate::native_paths::strip_prefix(&path, &workspace).unwrap();
    let nested_relative = crate::native_paths::strip_prefix(&path, &nested).unwrap();
    for _ in 0..3 {
        for spelling in [path.as_path(), workspace_relative, nested_relative] {
            candidate.with_text(spelling, |text| {
                assert_eq!(
                    text,
                    spelling
                        .to_string_lossy()
                        .replace('\\', "/")
                        .encode_utf16()
                        .collect::<Vec<_>>()
                );
            });
        }
    }
    assert_eq!(candidate.preparations.get(), 3);
}

#[test]
fn cache_does_not_conflate_lexically_equal_but_differently_spelled_paths() {
    let names = [
        Path::new("src//main.rs"),
        Path::new("src/main.rs"),
        Path::new("src/./main.rs"),
    ];
    assert_eq!(names[0], names[1]);
    let candidate = Candidate::new(names[0]);
    let mut patterns = GlobBuilder::default();
    add_glob(&mut patterns, "src/main.rs");
    let globs = patterns.finish();
    for _ in 0..2 {
        for name in names {
            assert_eq!(candidate.matches(&globs, name), legacy_match(&globs, name));
        }
    }
    assert_eq!(candidate.preparations.get(), 3);
}

#[test]
fn path_scopes_match_previous_behavior_with_overlapping_external_and_open_paths() {
    let workspace = std::env::current_dir().unwrap().join("search-路径");
    let external = workspace.parent().unwrap().join("outside");
    let absolute = external.to_string_lossy().replace('\\', "/");
    for pattern in [
        String::new(),
        ".".into(),
        "./src".into(),
        "./src/**/*.rs, *.toml".into(),
        "./src, ./src/deep/*.txt, *.rs".into(),
        "../outside/*.rs".into(),
        format!("{absolute}/**/*.rs, ./src/*.txt"),
        format!("{absolute}/**/*.txt, ./src, *.rs"),
    ] {
        let paths = SearchPaths::new(&workspace, &pattern);
        for path in [
            workspace.clone(),
            workspace.join("main.rs"),
            workspace.join("Cargo.toml"),
            workspace.join("src/main.rs"),
            workspace.join("src/deep/🦀.txt"),
            workspace.join("src-sibling/other.rs"),
            external.join("café.rs"),
            external.join("nested/test.txt"),
            external.join("nested/test.rs"),
        ] {
            let candidate = Candidate::new(&path);
            assert_eq!(
                paths.includes(&candidate),
                legacy_includes(&paths, &path),
                "includes {pattern:?} {path:?}"
            );
            assert_eq!(
                paths.includes_open(&candidate),
                legacy_includes_open(&paths, &path),
                "open {pattern:?} {path:?}"
            );
            assert_eq!(
                paths.excludes(&candidate, &workspace),
                legacy_excludes(&paths, &path, &workspace),
                "excludes {pattern:?} {path:?}"
            );
        }
    }
}

#[test]
fn empty_rules_and_unfiltered_paths_do_not_prepare_text() {
    let workspace = std::env::current_dir().unwrap();
    let path = workspace.join("any/file.rs");
    let candidate = Candidate::new(&path);
    for source in ["", "."] {
        let include = SearchPaths::new(&workspace, source);
        assert!(include.includes(&candidate));
        assert!(include.includes_open(&candidate));
    }
    let exclude = SearchPaths::new(&workspace, "");
    assert!(!exclude.excludes(&candidate, &workspace));
    assert!(!ExcludeRules(Vec::new()).excludes(&candidate, &workspace));
    assert!(!candidate.matches(&compile_patterns(""), &path));
    assert_eq!(candidate.preparations.get(), 0);
    assert!(candidate.prepared.borrow().first.is_none());
}

#[test]
fn include_reachability_prunes_siblings_at_every_anchored_component() {
    let root = std::env::current_dir().unwrap();
    for source in ["./cores/*/src", "./cores/*/src/"] {
        let paths = SearchPaths::new(&root, source);
        for relative in [
            "",
            "cores",
            "cores/arm",
            "cores/arm/src",
            "cores/arm/src/nested",
        ] {
            assert!(
                paths.may_descend(&Candidate::new(&root.join(relative))),
                "{source}: {relative}"
            );
        }
        for relative in [
            "elsewhere",
            "cores-extra",
            "cores/arm/artifacts",
            "cores/arm/src-extra",
        ] {
            assert!(
                !paths.may_descend(&Candidate::new(&root.join(relative))),
                "{source}: {relative}"
            );
        }
    }
    for source in ["", ".", "*.rs", "./cores/*/src, *.rs"] {
        let paths = SearchPaths::new(&root, source);
        assert!(
            paths.may_descend(&Candidate::new(&root.join("elsewhere/arbitrary"))),
            "{source}"
        );
    }
}

#[test]
fn include_reachability_is_compiled_once_and_only_when_directory_pruning_needs_it() {
    let root = std::env::current_dir().unwrap();
    for source in ["./cores/*/src", "./cores/*/src, *.rs"] {
        let paths = SearchPaths::new(&root, source);
        let path = root.join("cores/arm/src/file.rs");
        let candidate = Candidate::new(&path);
        assert!(paths.includes(&candidate));
        assert!(paths.includes_open(&candidate));
        let _ = paths.excludes(&candidate, &root);
        assert!(
            paths
                .descendants
                .iter()
                .all(|(_, cache)| cache.get().is_none())
        );
        assert!(paths.may_descend(&Candidate::new(&root.join("cores/arm"))));
        if paths.has_global {
            assert!(
                paths
                    .descendants
                    .iter()
                    .all(|(_, cache)| cache.get().is_none())
            );
        } else {
            let compiled = paths.descendants[0].1.get().unwrap().as_ref().unwrap() as *const _;
            assert!(paths.may_descend(&Candidate::new(&root.join("cores/rv"))));
            assert_eq!(
                compiled,
                paths.descendants[0].1.get().unwrap().as_ref().unwrap() as *const _
            );
        }
    }
}

#[test]
fn include_reachability_never_prunes_ancestors_of_an_exact_match() {
    let root = std::env::current_dir().unwrap();
    let names = [
        "cores",
        "arm",
        "src",
        "nested",
        "other",
        "Ω😀",
        "a,b",
        "a[1]",
        "foo.rs",
        "line\nname",
    ];
    for source in [
        "./cores/*/src",
        "./{cores,other}/*/{src,nested}",
        "./cores/{arm/src,nested/other}",
        "./cores/**/src",
        "./**/src",
        "./src/**/*.rs",
        "./cores/[a-z]*/src",
        "./cores/?/src",
        "./cores/??/src",
        "./Ω??/*",
        "./a[[]1]/**",
        "./src, ./cores/*/src",
        "./src, *.rs",
        ".",
        "",
        "src",
        "./src/../other",
        "./cores/{broken",
        "./cores/[broken",
        "./cores/foo**",
        "./cores/[a/]*/src",
        "./cores/***/src",
        "./cores/{,arm}/src",
        "./cores//src",
    ] {
        let paths = SearchPaths::new(&root, source);
        for first in names {
            for second in names {
                for third in names {
                    let path = root.join(first).join(second).join(third).join("file.rs");
                    if !paths.includes(&Candidate::new(&path)) {
                        continue;
                    }
                    for ancestor in path
                        .parent()
                        .unwrap()
                        .ancestors()
                        .take_while(|path| path.starts_with(&root))
                    {
                        assert!(
                            paths.may_descend(&Candidate::new(ancestor)),
                            "pruned {ancestor:?} for {source:?}: {path:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn include_reachability_handles_external_roots_unicode_and_complex_globs_conservatively() {
    let root = std::env::current_dir().unwrap().join("workspace");
    let outside = root.parent().unwrap().join("outside");
    let paths = SearchPaths::new(&root, "../outside/cores/*/src, ./local");
    assert!(paths.may_descend(&Candidate::new(&outside)));
    assert!(paths.may_descend(&Candidate::new(&outside.join("cores/Ω😀/src"))));
    assert!(!paths.may_descend(&Candidate::new(&outside.join("other"))));
    assert!(paths.may_descend(&Candidate::new(&root.join("local/deep"))));
    let paths = SearchPaths::new(&root, "./cores/??/src");
    assert!(paths.may_descend(&Candidate::new(&root.join("cores/😀"))));
    assert!(!paths.may_descend(&Candidate::new(&root.join("cores/abc"))));
    for pattern in ["./cores/**/src", "./cores/{arm/src,other/nested}"] {
        let paths = SearchPaths::new(&root, pattern);
        assert!(paths.may_descend(&Candidate::new(&root.join("cores/any/deep/subtree"))));
        assert!(!paths.may_descend(&Candidate::new(&root.join("elsewhere"))));
    }
    let paths = SearchPaths::new(&root, &format!("./{}file", "nested/".repeat(256)));
    assert!(
        paths.may_descend(&Candidate::new(&root.join("anywhere"))),
        "deep patterns use the safe fallback"
    );
}

#[test]
fn setting_overrides_conditional_siblings_and_changed_siblings_keep_their_behavior() {
    let root = tempfile::tempdir().unwrap();
    let files = BTreeMap::from([
        ("**/*.rs".into(), serde_json::json!(true)),
        (
            "**/*.js".into(),
            serde_json::json!({"when": "$(basename).ts"}),
        ),
    ]);
    let search = BTreeMap::from([("**/*.rs".into(), serde_json::json!(false))]);
    let rules = ExcludeRules::new(&files, &search);
    let path = root.path().join("café🦀.js");
    let sibling = path.with_extension("ts");
    let candidate = Candidate::new(&path);
    assert!(!rules.excludes(&candidate, root.path()));
    std::fs::write(&sibling, "sibling").unwrap();
    assert!(rules.excludes(&candidate, root.path()));
    assert!(legacy_settings_exclude(&rules, &path, root.path()));
    std::fs::remove_file(&sibling).unwrap();
    assert!(!rules.excludes(&candidate, root.path()));
    assert_eq!(candidate.preparations.get(), 1);
    for path in [
        root.path().join("visible.rs"),
        root.path().join("node_modules/index.js"),
        root.path().join(".git/config"),
        root.path().join("Thumbs.db"),
    ] {
        assert_eq!(
            rules.excludes(&Candidate::new(&path), root.path()),
            legacy_settings_exclude(&rules, &path, root.path())
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_preserve_lossy_matching_for_global_and_rooted_rules() {
    use std::os::unix::ffi::OsStringExt;
    let workspace = std::env::current_dir().unwrap();
    let path = workspace.join(std::ffi::OsString::from_vec(b"bad\xff\xfe.rs".to_vec()));
    let candidate = Candidate::new(&path);
    for pattern in ["*.rs", "./bad??.rs", "./bad�.rs", "./bad��.rs"] {
        let paths = SearchPaths::new(&workspace, pattern);
        assert_eq!(paths.includes(&candidate), legacy_includes(&paths, &path));
        assert_eq!(
            paths.includes_open(&candidate),
            legacy_includes_open(&paths, &path)
        );
        assert_eq!(
            paths.excludes(&candidate, &workspace),
            legacy_excludes(&paths, &path, &workspace)
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_native_prefix_aliases_and_unpaired_surrogates_keep_their_behavior() {
    use std::os::windows::ffi::OsStringExt;
    for (root, name) in [
        (r"C:\Repo", r"\\?\c:\REPO\src\main.rs"),
        (
            r"\\server\share\Repo",
            r"\\?\UNC\SERVER\share\REPO\src\main.rs",
        ),
    ] {
        let root = Path::new(root);
        let path = Path::new(name);
        let candidate = Candidate::new(path);
        for pattern in ["./src/*.rs", "./SRC/*.rs", "*.rs"] {
            let paths = SearchPaths::new(root, pattern);
            assert_eq!(paths.includes(&candidate), legacy_includes(&paths, path));
            assert_eq!(
                paths.excludes(&candidate, root),
                legacy_excludes(&paths, path, root)
            );
        }
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&[
        b'a' as u16,
        0xd800,
        b'.' as u16,
        b'r' as u16,
        b's' as u16,
    ]));
    let candidate = Candidate::new(&path);
    for pattern in ["*.rs", "a?.rs", "a�.rs"] {
        let globs = compile_patterns(pattern);
        assert_eq!(
            candidate.matches(&globs, &path),
            legacy_match(&globs, &path)
        );
    }
}

#[test]
#[ignore = "manual optimized search path filtering benchmark; run on one idle CPU"]
fn benchmark_prepared_search_paths() {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    let root = std::env::current_dir().unwrap();
    let files = (0..20)
        .map(|i| (format!("**/excluded-{i:02}/**"), serde_json::json!(true)))
        .collect();
    let rules = ExcludeRules::new(&files, &BTreeMap::new());
    let paths: Vec<_> = (0..8_192).map(|i| root.join(format!(
        "src/compiler/frontend/café🦀/generated_parser_component_{i:05}/analysis/semantic_checks/visitor.rs"
    ))).collect();
    let mut samples = [Vec::new(), Vec::new()];
    for sample in 0..11 {
        for order in 0..2 {
            let mode = (order + sample) % 2;
            let start = Instant::now();
            let mut excluded = 0;
            for path in &paths {
                excluded += usize::from(if mode == 0 {
                    legacy_settings_exclude(black_box(&rules), black_box(path), &root)
                } else {
                    rules.excludes(&Candidate::new(black_box(path)), &root)
                });
            }
            let elapsed = start.elapsed();
            assert_eq!(black_box(excluded), 0);
            if sample >= 2 {
                samples[mode].push(elapsed);
            }
        }
    }
    let median_ms = |times: &mut Vec<Duration>| {
        times.sort();
        times[times.len() / 2].as_secs_f64() * 1_000.0
    };
    println!(
        "{} paths, {} rules: repeated conversion {:.3} ms; prepared candidate {:.3} ms",
        paths.len(),
        rules
            .0
            .iter()
            .map(|(globs, _)| globs.0.len())
            .sum::<usize>(),
        median_ms(&mut samples[0]),
        median_ms(&mut samples[1])
    );
}
