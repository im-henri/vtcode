use crate::{
    app::{App, Tab},
    editor::Editor,
    remote,
    settings::{SettingsScope, default_files_exclude, default_search_exclude},
};
use std::{path::Path, sync::Arc, time::Duration};

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn search(app: &mut App, root: &Path) -> Vec<String> {
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(8), |app| !app.bg_global_search_running);
    assert!(!app.bg_global_search_running);
    assert!(
        app.global_search_error.is_none(),
        "{:?}",
        app.global_search_error
    );
    let mut paths = app
        .global_search_results
        .iter()
        .map(|hit| {
            app.remote_workspace.as_ref().map_or_else(
                || {
                    hit.file_path
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                },
                |remote| remote.remote_path(&hit.file_path).unwrap().display(),
            )
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn quick(app: &mut App, root: &Path, query: &str) -> Vec<String> {
    app.quick_open_query = query.into();
    app.update_quick_open();
    app.wait_for_background(Duration::from_secs(8), |app| !app.bg_quick_open_running);
    assert!(!app.bg_quick_open_running);
    let mut paths = app
        .quick_open_results
        .iter()
        .map(|path| {
            app.remote_workspace.as_ref().map_or_else(
                || {
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                },
                |remote| remote.remote_path(path).unwrap().display(),
            )
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

#[test]
fn workspace_exclusions_apply_defaults_inheritance_overrides_and_live_changes_locally_and_remotely()
{
    for hosted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "src/keep.rs",
            ".hidden/keep.rs",
            "build/out.rs",
            "target/out.rs",
            "cache/skip.rs",
            "node_modules/dependency.rs",
            "bower_components/dependency.rs",
            ".git/config",
            ".svn/entries",
            ".hg/repo",
            ".jj/repo",
            ".DS_Store",
            "Thumbs.db",
            "saved.code-search",
            "src/a.js",
            "src/a.ts",
            "src/b.js",
        ] {
            write(root.path(), path, "needle\n");
        }
        write(root.path(), ".gitignore", "build/\n.hidden/\ncache/\n");
        write(root.path(), ".ignore", "target/\n");
        write(root.path(), ".git/info/exclude", "src/\n");
        let remote = hosted.then(|| {
            remote::workspace::Workspace::new(remote::tests::connection(
                root.path(),
                Arc::default(),
                Arc::default(),
            ))
        });
        let mut app = App::new_deferred(
            remote
                .as_ref()
                .map_or_else(|| root.path().to_owned(), |r| r.view_root.clone()),
        );
        app.remote_workspace = remote.clone();
        app.source_control_enabled = false;
        app.global_search_query = "needle".into();
        // Even imported VS Code ignore toggles must not turn ignore files on.
        app.settings_workspace.search_use_ignore_files = Some(true);
        app.settings_workspace.search_use_global_ignore_files = Some(true);
        app.settings_workspace.search_use_parent_ignore_files = Some(true);
        assert_eq!(
            search(&mut app, root.path()),
            [
                ".hidden/keep.rs",
                "build/out.rs",
                "cache/skip.rs",
                "src/a.js",
                "src/a.ts",
                "src/b.js",
                "src/keep.rs",
                "target/out.rs"
            ]
        );
        assert_eq!(
            quick(&mut app, root.path(), "out"),
            ["build/out.rs", "target/out.rs"]
        );
        assert!(quick(&mut app, root.path(), "dependency").is_empty());
        assert_eq!(
            quick(&mut app, root.path(), "keep"),
            [".hidden/keep.rs", "src/keep.rs"]
        );
        app.refresh_explorer_tree_async();
        app.wait_for_background(Duration::from_secs(8), |a| {
            !a.bg_explorer_refresh_running && !a.bg_explorer_refresh_pending
        });
        for name in [
            "node_modules",
            "bower_components",
            "saved.code-search",
            "build",
            "target",
            ".hidden",
        ] {
            assert!(
                app.explorer.entries.iter().any(|e| e.name == name),
                "{name}, remote={hosted}"
            );
        }
        for name in [".git", ".svn", ".hg", ".jj", ".DS_Store", "Thumbs.db"] {
            assert!(
                !app.explorer.entries.iter().any(|e| e.name == name),
                "{name}, remote={hosted}"
            );
        }
        app.settings_global
            .files_exclude
            .insert("**/cache".into(), serde_json::json!(true));
        app.settings_global.files_exclude.insert(
            "**/*.js".into(),
            serde_json::json!({"when":"$(basename).ts"}),
        );
        app.settings_workspace
            .search_exclude
            .insert("**/node_modules".into(), serde_json::json!(false));
        app.settings_workspace
            .search_exclude
            .insert("**/.git".into(), serde_json::json!(false));
        app.apply_effective_settings();
        assert_eq!(
            search(&mut app, root.path()),
            [
                ".git/config",
                ".hidden/keep.rs",
                "build/out.rs",
                "node_modules/dependency.rs",
                "src/a.ts",
                "src/b.js",
                "src/keep.rs",
                "target/out.rs"
            ]
        );
        assert_eq!(
            quick(&mut app, root.path(), "dependency"),
            ["node_modules/dependency.rs"]
        );
        assert!(quick(&mut app, root.path(), "skip").is_empty());
        app.wait_for_background(Duration::from_secs(8), |a| {
            !a.bg_explorer_refresh_running && !a.bg_explorer_refresh_pending
        });
        assert!(
            !app.explorer
                .entries
                .iter()
                .any(|e| e.name == "cache" || e.name == ".git")
        );
        // Open models are filtered beside the disk, including conditional rules
        // whose sibling is created or removed between otherwise identical queries.
        let mut editor = Editor::test_with_text("unsaved needle\n");
        editor.file_path = Some(remote.as_ref().map_or_else(
            || root.path().join("src/b.js"),
            |r| r.target_key("src/b.js").unwrap(),
        ));
        app.tabs.push(Tab::new_file(editor));
        assert_eq!(quick(&mut app, root.path(), "b.js"), ["src/b.js"]);
        write(root.path(), "src/b.ts", "unmatched\n");
        assert!(quick(&mut app, root.path(), "b.js").is_empty());
        assert!(!search(&mut app, root.path()).contains(&"src/b.js".into()));
        std::fs::remove_file(root.path().join("src/b.ts")).unwrap();
        assert_eq!(quick(&mut app, root.path(), "b.js"), ["src/b.js"]);
        assert!(search(&mut app, root.path()).contains(&"src/b.js".into()));
        if let Some(remote) = remote {
            remote.client.call(remote::Command::Shutdown).unwrap();
        }
    }
}

#[test]
fn workspace_exclusions_keep_settings_globs_rooted_and_filter_explicit_roots() {
    let root = tempfile::tempdir().unwrap();
    for path in ["cache/file.txt", "nested/cache/file.txt"] {
        write(root.path(), path, "needle");
    }
    let mut app = App::new_deferred(root.path().to_owned());
    app.source_control_enabled = false;
    app.global_search_query = "needle".into();
    app.settings_workspace
        .files_exclude
        .insert("cache".into(), serde_json::json!(true));
    app.search_include_pattern = "./cache".into();
    assert!(search(&mut app, root.path()).is_empty());
    app.search_include_pattern = "./cache/file.txt".into();
    assert!(search(&mut app, root.path()).is_empty());
    app.search_include_pattern = "./nested".into();
    assert_eq!(search(&mut app, root.path()), ["nested/cache/file.txt"]);
    app.settings_workspace
        .search_exclude
        .insert("cache".into(), serde_json::json!(false));
    app.search_include_pattern = "./cache".into();
    assert_eq!(search(&mut app, root.path()), ["cache/file.txt"]);
}

#[test]
fn workspace_exclusions_history_uses_absolute_patterns_on_the_native_host() {
    for hosted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        for path in ["src/keep.rs", "node_modules/dependency.rs"] {
            write(root.path(), path, "");
        }
        let remote = hosted.then(|| {
            remote::workspace::Workspace::new(remote::tests::connection(
                root.path(),
                Arc::default(),
                Arc::default(),
            ))
        });
        let mut app = App::new_deferred(
            remote
                .as_ref()
                .map_or_else(|| root.path().to_owned(), |r| r.view_root.clone()),
        );
        app.remote_workspace = remote.clone();
        app.source_control_enabled = false;
        app.recent_files = ["src/keep.rs", "node_modules/dependency.rs"]
            .into_iter()
            .map(|path| {
                remote
                    .as_ref()
                    .map_or_else(|| root.path().join(path), |r| r.target_key(path).unwrap())
            })
            .collect();
        assert_eq!(
            quick(&mut app, root.path(), ""),
            ["node_modules/dependency.rs", "src/keep.rs"]
        );
        app.settings_workspace
            .search_exclude
            .insert("**/node_modules/**".into(), serde_json::json!(true));
        assert_eq!(quick(&mut app, root.path(), ""), ["src/keep.rs"]);
        if let Some(remote) = remote {
            let outside = std::path::PathBuf::from("another-workspace-recent.rs");
            app.recent_files.push(outside.clone());
            app.quick_open_query.clear();
            app.update_quick_open();
            app.wait_for_background(Duration::from_secs(8), |app| !app.bg_quick_open_running);
            assert!(!app.bg_quick_open_running);
            assert!(app.quick_open_results.contains(&outside));
            remote.client.call(remote::Command::Shutdown).unwrap();
        }
    }
}

#[test]
fn workspace_exclusions_merge_by_pattern_without_polluting_stored_layers_or_search_display() {
    let mut app = App::test_new();
    assert!(app.settings_global.files_exclude.is_empty());
    assert!(app.settings_workspace.search_exclude.is_empty());
    assert_eq!(
        app.settings_effective.files_exclude,
        default_files_exclude()
    );
    assert_eq!(
        app.settings_effective.search_exclude,
        default_search_exclude()
    );
    app.settings_global
        .files_exclude
        .insert("**/global".into(), serde_json::json!(true));
    app.settings_global
        .search_exclude
        .insert("**/node_modules".into(), serde_json::json!(false));
    app.settings_workspace
        .search_exclude
        .insert("**/node_modules".into(), serde_json::json!(true));
    app.settings_scope = SettingsScope::Global;
    let user = app.selected_scope_settings();
    assert_eq!(user.search_exclude["**/node_modules"], false);
    app.settings_scope = SettingsScope::Workspace;
    let workspace = app.selected_scope_settings();
    assert_eq!(workspace.search_exclude["**/node_modules"], true);
    let items = app.settings_items();
    let (_, search, _) = items
        .iter()
        .find(|(_, _, id)| id == "search_exclude")
        .unwrap();
    let display: crate::settings::ExcludePatterns = serde_json::from_str(search).unwrap();
    assert_eq!(display.len(), 3);
    assert!(!display.contains_key("**/.git") && !display.contains_key("**/global"));
    let rules =
        crate::search_paths::ExcludeRules::new(&workspace.files_exclude, &workspace.search_exclude);
    assert!(rules.excludes_tree(Path::new("workspace/global/file"), Path::new("workspace")));
}

#[test]
fn workspace_exclusions_explorer_filters_cached_raw_entries_and_siblings() {
    let root = tempfile::tempdir().unwrap();
    for path in ["src/a.js", "src/a.ts", "src/b.js"] {
        write(root.path(), path, "");
    }
    let mut explorer = crate::explorer::FileExplorer::new_deferred(root.path().to_owned());
    let mut patterns = default_files_exclude();
    patterns.insert(
        "**/*.js".into(),
        serde_json::json!({"when":"$(basename).ts"}),
    );
    explorer.set_files_exclude(&patterns);
    let scan = |explorer: &mut crate::explorer::FileExplorer| {
        let request = explorer.headless_request(
            std::iter::once(root.path().join("src")).collect(),
            crate::explorer::ExplorerSortMode::Name,
            None,
            crate::background::CancellationToken::new(),
        );
        let (_, entries, _) = crate::explorer::FileExplorer::build_refresh_entries(request);
        entries
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(scan(&mut explorer), ["src", "a.ts", "b.js"]);
    patterns.insert("**/*.js".into(), serde_json::json!(false));
    explorer.set_files_exclude(&patterns);
    assert_eq!(scan(&mut explorer), ["src", "a.js", "a.ts", "b.js"]);
    // Native case folding also applies to the cached sibling lookup.
    patterns.insert(
        "**/*.js".into(),
        serde_json::json!({"when":"$(basename).TS"}),
    );
    explorer.set_files_exclude(&patterns);
    let expected = if cfg!(any(windows, target_os = "macos")) {
        vec!["src", "a.ts", "b.js"]
    } else {
        vec!["src", "a.js", "a.ts", "b.js"]
    };
    assert_eq!(scan(&mut explorer), expected);
}
