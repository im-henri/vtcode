//! End-to-end disk search workload: narrow source includes in a broad workspace.
use crate::{
    background::CancellationToken,
    remote::{
        Reply,
        search::{self, Query},
    },
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn query(include: &str) -> Query {
    Query {
        stream: false,
        text: "CoreProper".into(),
        case_sensitive: false,
        whole_word: false,
        regex: false,
        include: include.into(),
        exclude: String::new(),
        limit: 20_000,
        files_exclude: Default::default(),
        search_exclude: Default::default(),
        use_ignores: true,
        parent_ignores: false,
        global_ignores: false,
        follow_links: true,
        open_files: Vec::new(),
    }
}

fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    path
}

fn fixture(root: &Path, vendor_trees: usize, artifact_dirs: usize) {
    for file in 0..35 {
        let text = format!(
            "{}{}",
            "ordinary configuration\n".repeat(40),
            "CoreProperty MaxInstructionWidth = 16;\n".repeat(5 + usize::from(file < 2))
        );
        write(
            root,
            &format!("cores/core_{}/src/entry_{file}.cps", file / 5),
            &text,
        );
    }
    // Deliberately ordinary names, not default- or git-ignored directories.
    // Search must avoid enumerating these rather than hiding them after a walk.
    for tree in 0..vendor_trees {
        for branch in 0..8 {
            for file in 0..8 {
                write(
                    root,
                    &format!("vendor_{tree}/branch_{branch}/data_{file}.txt"),
                    "CoreProperty unrelated\n",
                );
            }
        }
    }
    for core in 0..7 {
        for branch in 0..artifact_dirs {
            for file in 0..4 {
                write(
                    root,
                    &format!("cores/core_{core}/artifacts_{branch}/data_{file}.txt"),
                    "CoreProperty unrelated\n",
                );
            }
        }
    }
}

#[test]
#[ignore = "manual workspace search benchmark; run alone on an idle machine"]
fn benchmark_scoped_workspace_search() {
    let workspace = tempfile::tempdir().unwrap();
    fixture(workspace.path(), 64, 24);
    crate::search_walk::take_visited_entries();
    let mut samples = Vec::new();
    for sample in 0..9 {
        let started = Instant::now();
        let Reply::Search { hits, truncated } = search::execute(
            workspace.path(),
            query("./cores/*/src/"),
            &CancellationToken::new(),
        )
        .unwrap() else {
            panic!("search response");
        };
        let elapsed = started.elapsed();
        assert_eq!(hits.len(), 177);
        assert!(!truncated);
        assert_eq!(
            hits.iter()
                .map(|hit| &hit.path)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            35
        );
        if sample > 1 {
            samples.push(elapsed);
        }
    }
    samples.sort();
    let visited = crate::search_walk::take_visited_entries() / 9;
    let previous_visits = legacy_paths(workspace.path(), &query("./cores/*/src/")).1;
    println!(
        "scoped workspace search: 4,803 files, 177 hits in 35 source files; visited entries {previous_visits} -> {visited}; median {:.3} ms; samples {:?}",
        samples[samples.len() / 2].as_secs_f64() * 1000.0,
        samples
    );
    assert!(samples[samples.len() / 2] < Duration::from_secs(30));
}

#[test]
#[ignore = "manual paired complex-include benchmark; run alone on an idle machine"]
fn benchmark_complex_workspace_includes() {
    let workspace = tempfile::tempdir().unwrap();
    fixture(workspace.path(), 64, 24);
    let mut many = (0..32)
        .map(|i| format!("./unused_{i}/**/*.{ext}", ext = "{cps,rs,ts}"))
        .collect::<Vec<_>>();
    many.push("./cores/*/src/".into());
    let cases = [
        ("rooted", "./cores/*/src/".to_owned()),
        (
            "multiple",
            "./cores/{core_0,core_1}/src, ./cores/core_[2-4]/src/**, ./cores/core_?/src/*.cps"
                .into(),
        ),
        ("many", many.join(", ")),
        (
            "unrooted",
            "cores/*/src/**/*.cps, **/core_[0-2]/src/entry_*.cps, **/unused*/{src,test}/*.{rs,ts}"
                .into(),
        ),
        ("extension", "*.cps".into()),
    ];
    let paths = (0..4096)
        .map(|i| {
            workspace.path().join(if i % 3 == 0 {
                format!("cores/core_{}/src/entry_{}.cps", i % 7, i % 35)
            } else {
                format!(
                    "vendor_{}/branch_{}/generated/components/analysis/data_{i}.txt",
                    i % 64,
                    i % 8
                )
            })
        })
        .collect::<Vec<_>>();
    let mut report = Vec::new();
    for (name, include) in cases {
        let plan_started = Instant::now();
        for _ in 0..20 {
            std::hint::black_box(crate::search_paths::SearchPaths::new(
                workspace.path(),
                &include,
            ));
        }
        let plan_ms = plan_started.elapsed().as_secs_f64() * 1000.0 / 20.0;
        let plans = crate::search_paths::SearchPaths::new(workspace.path(), &include);
        let mut filters = Vec::new();
        let mut disk = Vec::new();
        crate::search_walk::take_visited_entries();
        for sample in 0..7 {
            let started = Instant::now();
            let matched = paths
                .iter()
                .filter(|path| plans.includes(&crate::search_paths::Candidate::new(path)))
                .count();
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(matched, 1366, "{name}");
            if sample >= 2 {
                filters.push(elapsed);
            }
            let started = Instant::now();
            let Reply::Search { hits, truncated } =
                search::execute(workspace.path(), query(&include), &CancellationToken::new())
                    .unwrap()
            else {
                panic!("search response");
            };
            assert_eq!(hits.len(), 177, "{name}");
            assert!(!truncated);
            if sample >= 2 {
                disk.push(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
        filters.sort_by(f64::total_cmp);
        disk.sort_by(f64::total_cmp);
        report.push(serde_json::json!({
            "case": name, "include": include, "buildMs": plan_ms,
            "filterMedianMs": filters[2], "searchMedianMs": disk[2],
            "filterSamplesMs": filters, "searchSamplesMs": disk,
            "visitedEntries": crate::search_walk::take_visited_entries() / 7,
            "files": 4803, "hits": 177, "candidatePaths": paths.len(),
        }));
    }
    let report = serde_json::to_string_pretty(&report).unwrap();
    println!("{report}");
    if let Some(path) = std::env::var_os("VTCODE_SEARCH_BENCH_REPORT") {
        std::fs::write(path, report).unwrap();
    }
}

fn legacy_paths(root: &Path, query: &Query) -> (std::collections::BTreeSet<PathBuf>, usize) {
    use crate::search_paths::{Candidate, ExcludeRules, SearchPaths};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let include = SearchPaths::new(root, &query.include);
    let exclude = Arc::new(SearchPaths::new(root, &query.exclude));
    let settings = Arc::new(ExcludeRules::new(
        &query.files_exclude,
        &query.search_exclude,
    ));
    let visited = Arc::new(AtomicUsize::new(0));
    let mut paths = std::collections::BTreeSet::new();
    for folder in include.roots() {
        let mut walk = ignore::WalkBuilder::new(folder);
        let workspace = root.to_owned();
        let excludes = exclude.clone();
        let settings = settings.clone();
        let visited = visited.clone();
        walk.hidden(false)
            .require_git(false)
            .parents(false)
            .ignore(false)
            .git_ignore(false)
            .git_exclude(false)
            .git_global(false)
            .follow_links(query.follow_links)
            .filter_entry(move |entry| {
                visited.fetch_add(1, Ordering::Relaxed);
                let candidate = Candidate::new(entry.path());
                !settings.excludes(&candidate, &workspace)
                    && !excludes.excludes(&candidate, &workspace)
            });
        for entry in walk
            .build()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        {
            let candidate = Candidate::new(entry.path());
            if include.includes(&candidate) && !exclude.excludes(&candidate, root) {
                paths.insert(entry.into_path());
            }
        }
    }
    (paths, visited.load(Ordering::Relaxed))
}

fn optimized_paths(
    root: &Path,
    query: &Query,
    cancel: &CancellationToken,
) -> std::collections::BTreeSet<PathBuf> {
    use crate::search_paths::{ExcludeRules, SearchPaths};
    use std::sync::Arc;
    crate::search_walk::paths(
        root,
        Arc::new(SearchPaths::new(root, &query.include)),
        Arc::new(SearchPaths::new(root, &query.exclude)),
        Arc::new(ExcludeRules::new(
            &query.files_exclude,
            &query.search_exclude,
        )),
        crate::search_walk::Options {
            follow_links: query.follow_links,
        },
        cancel,
    )
    .collect()
}

#[test]
fn workspace_search_finds_existing_and_generated_files_without_implicit_exclusions() {
    use crate::{app::App, remote};
    use std::sync::Arc;

    for use_remote in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let initial = [
            "my/first/path/build/existing.txt",
            "my/second/path/regular.txt",
            "my/second/path/node_modules/dependency.txt",
            "my/third/path/cache/ignored.txt",
            "my/third/path/.hidden/hidden.txt",
            "my/third/path/.git/metadata.txt",
        ];
        for relative in initial.into_iter().chain(["outside/existing.txt"]) {
            write(root.path(), relative, "mysearch value\n");
        }
        write(root.path(), ".gitignore", "build/\nnode_modules/\n");
        write(root.path(), ".ignore", "cache/\n");
        let remote = use_remote.then(|| remote::workspace::Workspace::new(
            remote::tests::connection(root.path(), Arc::default(), Arc::default()),
        ));
        let mut app = App::new_deferred(remote.as_ref()
            .map_or_else(|| root.path().to_owned(), |remote| remote.view_root.clone()));
        app.source_control_enabled = false;
        app.remote_workspace = remote.clone();
        app.global_search_query = "mysearch".into();
        app.search_include_pattern = "./my/*/path".into();
        app.settings_workspace.search_use_ignore_files = Some(true);
        app.settings_workspace.search_use_parent_ignore_files = Some(true);
        app.settings_workspace.search_use_global_ignore_files = Some(true);
        app.settings_workspace.files_exclude.insert("**/cache".into(), serde_json::json!(true));
        app.settings_workspace.search_exclude.insert("**/build".into(), serde_json::json!(true));
        let search = |app: &mut App, expected: &[&str]| {
            app.update_global_search();
            app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
            assert!(!app.bg_global_search_running);
            assert!(app.global_search_error.is_none(), "{:?}", app.global_search_error);
            assert!(!app.global_search_truncated);
            let mut actual = app.global_search_results.iter().map(|hit| {
                remote.as_ref().map_or_else(
                    || hit.file_path.strip_prefix(root.path()).unwrap().to_string_lossy().replace('\\', "/"),
                    |remote| remote.remote_path(&hit.file_path).unwrap().display(),
                )
            }).collect::<Vec<_>>();
            actual.sort();
            let mut expected = expected.to_vec();
            expected.sort();
            assert_eq!(actual, expected, "remote={use_remote}, include={:?}", app.search_include_pattern);
        };
        search(&mut app, &[initial[1], initial[4]]);
        for pattern in ["**/cache", "**/build", "**/node_modules", "**/.git"] {
            app.settings_workspace.search_exclude.insert(pattern.into(), serde_json::json!(false));
        }
        search(&mut app, &initial);
        let generated = ["my/first/path/build/generated.txt", "my/new/path/build/generated.txt"];
        for relative in generated { write(root.path(), relative, "mysearch generated\n"); }
        app.global_search_query = "no such match".into();
        search(&mut app, &[]);
        app.global_search_query = "mysearch".into();
        let expected = initial.into_iter().chain(generated).collect::<Vec<_>>();
        search(&mut app, &expected);
        app.search_exclude_pattern = "**/build/**".into();
        search(&mut app, &initial[1..]);
        app.search_exclude_pattern.clear();
        app.search_include_pattern.clear();
        search(&mut app, &expected.into_iter().chain(["outside/existing.txt"]).collect::<Vec<_>>());
        if let Some(remote) = remote { remote.client.call(remote::Command::Shutdown).unwrap(); }
    }
}

#[test]
fn cached_include_plans_keep_file_discovery_and_sibling_checks_live_without_ignore_files() {
    let workspace = tempfile::tempdir().unwrap();
    write(
        workspace.path(),
        "cores/one/src/a.js",
        "CoreProperty value\n",
    );
    let mut request = query("./cores/*/src");
    request.search_exclude.insert(
        "**/*.js".into(),
        serde_json::json!({"when":"$(basename).ts"}),
    );
    let hits = |request: &Query| {
        let mut current = query(&request.include);
        current.search_exclude = request.search_exclude.clone();
        let Reply::Search { hits, truncated } =
            search::execute(workspace.path(), current, &CancellationToken::new()).unwrap()
        else {
            panic!("search response");
        };
        assert!(!truncated);
        hits.len()
    };
    assert_eq!(hits(&request), 1);
    write(workspace.path(), "cores/one/src/a.ts", "no match\n");
    assert_eq!(
        hits(&request),
        0,
        "conditional exclusions see newly created siblings"
    );
    write(
        workspace.path(),
        "cores/one/src/b.js",
        "CoreProperty value\n",
    );
    assert_eq!(
        hits(&request),
        1,
        "new files are discovered with the same include plan"
    );
    std::fs::remove_file(workspace.path().join("cores/one/src/a.ts")).unwrap();
    assert_eq!(hits(&request), 2, "removed siblings are not cached");
    write(workspace.path(), ".gitignore", "b.js\n");
    assert_eq!(
        hits(&request),
        2,
        "ignore files never filter workspace search"
    );
}

#[test]
fn scoped_workspace_search_prunes_unrelated_entries_before_exclusion_and_file_reads() {
    let workspace = tempfile::tempdir().unwrap();
    fixture(workspace.path(), 12, 4);
    let query = query("./cores/*/src/");
    let (expected, previous_visits) = legacy_paths(workspace.path(), &query);
    crate::search_walk::take_visited_entries();
    assert_eq!(
        optimized_paths(workspace.path(), &query, &CancellationToken::new()),
        expected
    );
    let visits = crate::search_walk::take_visited_entries();
    assert_eq!(expected.len(), 35);
    assert!(visits < 120, "unrelated subtrees were enumerated: {visits}");
    assert!(
        previous_visits > visits * 5,
        "{previous_visits} -> {visits}"
    );
}

#[test]
fn scoped_workspace_traversal_preserves_globs_and_sibling_excludes_without_ignore_files() {
    let parent = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let root = parent.path().join("workspace");
    for file in [
        "cores/arm/src/a.cps",
        "cores/arm/src/nested/b.cps",
        "cores/arm/src/ignored.cps",
        "cores/arm/src/node_modules/a.cps",
        "cores/arm/artifacts/a.cps",
        "cores/rv/src/a.cps",
        "cores/rv/src/pair.js",
        "cores/rv/src/pair.ts",
        "cores/rv/src/unpaired.js",
        "elsewhere/src/a.cps",
        "outside.cps",
        "a[1]/src/a.cps",
        ".hidden/src/a.cps",
        "cores/arm/src/.vtcode-format-test/copy.cps",
    ] {
        write(&root, file, "CoreProperty value\n");
    }
    write(parent.path(), "outside/src/a.cps", "CoreProperty value\n");
    write(&root, ".gitignore", "ignored.cps\n");
    write(&root, "cores/rv/.ignore", "a.cps\n");
    for include in [
        "",
        ".",
        "./cores/*/src",
        "./cores/*/src/",
        "./cores/{arm,rv}/src",
        "./cores/**/src",
        "./cores/*/src/*.js",
        "./a[[]1]/src",
        "./.hidden",
        "./cores/*/src, *.cps",
        "./cores/*/src, ./cores/arm/src",
        "../outside/src, ./cores/arm/src",
        "./outside.cps",
    ] {
        for use_ignores in [true, false] {
            let mut query = query(include);
            query.use_ignores = use_ignores;
            query.exclude = "./cores/arm/src/nested".into();
            query.search_exclude.insert(
                "**/*.js".into(),
                serde_json::json!({"when":"$(basename).ts"}),
            );
            let expected = legacy_paths(&root, &query).0;
            let actual = optimized_paths(&root, &query, &CancellationToken::new());
            assert_eq!(actual, expected, "{include}, ignores={use_ignores}");
        }
    }
    let mut query = query("./outside.cps");
    query.include = root.join("outside.cps").to_string_lossy().into_owned();
    query.exclude = "./outside.cps".into();
    assert!(optimized_paths(&root, &query, &CancellationToken::new()).is_empty());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(optimized_paths(&root, &query, &cancelled).is_empty());
    assert_eq!(
        search::execute(&root, query, &cancelled)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
}

#[test]
fn scoped_workspace_search_keeps_unsaved_and_ignored_models_locally_and_remotely() {
    use crate::{
        app::{App, Tab},
        editor::Editor,
        remote::{self, RemotePath},
    };
    use std::sync::Arc;
    for use_remote in [false, true] {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "cores/arm/src/a.cps", "CoreProperty disk\n");
        write(
            root.path(),
            "cores/arm/src/ignored.cps",
            "CoreProperty ignored\n",
        );
        write(
            root.path(),
            "cores/arm/build/out.cps",
            "CoreProperty build\n",
        );
        write(root.path(), "elsewhere/out.cps", "CoreProperty other\n");
        write(root.path(), ".gitignore", "ignored.cps\n");
        let remote = use_remote.then(|| {
            remote::workspace::Workspace::new(remote::tests::connection(
                root.path(),
                Arc::default(),
                Arc::default(),
            ))
        });
        let mut app = App::new_deferred(
            remote
                .as_ref()
                .map_or_else(|| root.path().to_owned(), |remote| remote.view_root.clone()),
        );
        app.source_control_enabled = false;
        app.remote_workspace = remote.clone();
        app.global_search_query = "CoreProper".into();
        app.search_include_pattern = "./cores/*/src/".into();
        app.search_case_sensitive = false;
        let search = |app: &mut App| {
            app.update_global_search();
            app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
            assert!(
                app.global_search_error.is_none(),
                "{:?}",
                app.global_search_error
            );
        };
        search(&mut app);
        assert_eq!(app.global_search_results.len(), 2, "remote={use_remote}");
        for (relative, text) in [
            ("cores/arm/src/ignored.cps", "CoreProperty draft"),
            ("cores/arm/src/a.cps", "unsaved without a match"),
            ("elsewhere/out.cps", "CoreProperty outside scope"),
        ] {
            let mut editor = Editor::test_with_text(text);
            editor.file_path = Some(remote.as_ref().map_or_else(
                || root.path().join(relative),
                |remote| remote.view_key(&RemotePath::new(relative.as_bytes().to_vec()).unwrap()),
            ));
            app.tabs.push(Tab::new_file(editor));
        }
        search(&mut app);
        assert_eq!(
            app.global_search_results.len(),
            1,
            "open models take precedence; remote={use_remote}"
        );
        assert_eq!(
            &*app.global_search_results[0].line_text,
            "CoreProperty draft"
        );
        if let Some(remote) = remote {
            remote.client.call(remote::Command::Shutdown).unwrap();
        }
    }
}
