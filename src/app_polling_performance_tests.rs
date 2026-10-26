use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::PeriodicDiskPaths;
use crate::app::{App, EditorGroup, Tab};
use crate::editor::Editor;

fn tab(path: impl AsRef<Path>) -> Tab {
    let mut editor = Editor::test_with_text("fn sample() {}\n");
    editor.file_path = Some(path.as_ref().to_path_buf());
    Tab::new_file(editor)
}

// The original loop rebuilt this set on every input/animation frame.
fn uncached_disk_paths(app: &App) -> Vec<PathBuf> {
    app.tabs
        .iter()
        .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
        .filter(|tab| {
            !matches!(
                tab.kind,
                crate::app::TabKind::Diff(_)
                    | crate::app::TabKind::GitRevision
                    | crate::app::TabKind::CommitDetails
            )
        })
        .filter_map(|tab| tab.editor.file_path.as_ref())
        .filter(|path| {
            !app.workspace_watcher_active
                || !crate::background::workspace_path_is_watched(&app.explorer.root, path)
        })
        .cloned()
        .collect::<rustc_hash::FxHashSet<_>>()
        .into_iter()
        .collect()
}

#[test]
fn remote_disk_inventory_reuses_coverage_and_keeps_unwatched_and_alias_fallbacks() {
    use crate::remote::{self, Checkpoint, RemotePath};
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let remote = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let source = RemotePath::new(b"src/main.rs".to_vec()).unwrap();
    let generated = RemotePath::new(b"target/generated.rs".to_vec()).unwrap();
    let outside = RemotePath::absolute(b"/elsewhere/outside.rs").unwrap();
    let alias = RemotePath::new(b"alias.rs".to_vec()).unwrap();
    let mut app = App::new_deferred(remote.view_root.clone());
    app.groups.clear();
    app.tabs = [&source, &generated, &outside, &alias]
        .into_iter()
        .map(|path| {
            let mut tab = tab(remote.view_key(path));
            tab.editor.remote_checkpoint = Some(Arc::new(Checkpoint {
                revision: remote::revision(&tab.editor.rope, "UTF-8"),
                conflicted: false,
                identity: Some(if path == &alias {
                    outside.clone()
                } else {
                    path.clone()
                }),
            }));
            tab
        })
        .collect();
    app.remote_workspace = Some(remote.clone());
    app.workspace_watcher_active = true;
    let mut cache = PeriodicDiskPaths::default();
    let expected = [&generated, &outside, &alias]
        .into_iter()
        .map(|path| remote.view_key(path))
        .collect::<Vec<_>>();
    assert_eq!(cache.get(&app), expected);
    let allocation = cache.get(&app).as_ptr();
    for index in 0..100 {
        app.tabs[0].editor.content_version += 1;
        app.tabs[0].editor.cursor_col = index;
        app.tabs[0].editor.scroll_offset = index;
        assert_eq!(cache.get(&app).as_ptr(), allocation);
        assert_eq!(cache.rebuilds, 1);
    }
    // A save/reload can replace the canonical identity without renaming a tab.
    app.tabs[3].editor.remote_checkpoint = Some(Arc::new(Checkpoint {
        revision: remote::revision(&app.tabs[3].editor.rope, "UTF-8"),
        conflicted: false,
        identity: Some(alias),
    }));
    assert_eq!(cache.get(&app), &expected[..2]);
    app.workspace_watcher_active = false;
    assert_eq!(cache.get(&app).len(), 4);
    app.tabs[0].kind = crate::app::TabKind::GitRevision;
    assert_eq!(
        cache.get(&app).len(),
        3,
        "Immutable snapshots need no disk checks"
    );
    let rebuilds = cache.rebuilds;
    for _ in 0..10 {
        assert_eq!(cache.get(&app).len(), 3);
    }
    assert_eq!(cache.rebuilds, rebuilds);
}

fn assert_disk_paths(cache: &mut PeriodicDiskPaths, app: &App, expected: &[PathBuf]) {
    let actual: rustc_hash::FxHashSet<_> = cache.get(app).iter().cloned().collect();
    assert_eq!(actual, expected.iter().cloned().collect());
    assert_eq!(
        cache.get(app).len(),
        actual.len(),
        "duplicate paths must be polled only once"
    );
    assert_eq!(actual, uncached_disk_paths(app).into_iter().collect());
}

#[test]
fn workbench_disk_inventory_tracks_watcher_coverage_and_split_panes() {
    let mut app = App::test_new();
    let root = PathBuf::from("workspace");
    let source = root.join("src/main.rs");
    let build = root.join("target/generated.rs");
    let outside = PathBuf::from("other/notes.txt");
    app.explorer.root = root.clone();
    app.workspace_watcher_active = true;
    app.tabs = vec![tab(&source), tab(&build), Tab::new_file(Editor::new())];
    let mut group = EditorGroup::new();
    group.tabs = vec![
        tab(&outside),
        tab(&build),
        tab(root.join("target/./generated.rs")),
    ];
    app.groups = vec![group];
    let mut cache = PeriodicDiskPaths::default();

    assert_disk_paths(&mut cache, &app, &[build.clone(), outside.clone()]);
    app.workspace_watcher_active = false;
    assert_disk_paths(
        &mut cache,
        &app,
        &[source.clone(), build.clone(), outside.clone()],
    );
    app.workspace_watcher_active = true;
    app.explorer.root = "other".into();
    assert_disk_paths(&mut cache, &app, &[source, build]);
    app.tabs.clear();
    app.groups.clear();
    assert_disk_paths(&mut cache, &app, &[]);
}

#[test]
fn workbench_disk_inventory_reuses_paths_until_files_change() {
    let mut app = App::test_new();
    app.workspace_watcher_active = false;
    app.tabs = vec![tab("a.rs"), tab("b.rs")];
    app.groups.clear();
    let mut cache = PeriodicDiskPaths::default();
    let allocation = cache.get(&app).as_ptr();
    for index in 0..32 {
        app.tabs[0].editor.content_version += 1;
        app.tabs[0].editor.cursor_col = index;
        app.tabs[0].editor.scroll_offset = index;
        app.active_tab = index % 2;
        assert_eq!(cache.get(&app).as_ptr(), allocation);
    }
    assert_eq!(cache.rebuilds, 1);

    // A same-sized inventory can still refer to different files.
    app.tabs[1].editor.file_path = Some("renamed.rs".into());
    assert_disk_paths(&mut cache, &app, &["a.rs".into(), "renamed.rs".into()]);
    app.tabs[0] = tab("replacement.rs");
    assert_disk_paths(
        &mut cache,
        &app,
        &["replacement.rs".into(), "renamed.rs".into()],
    );
    let mut group = EditorGroup::new();
    group.tabs.push(app.tabs.remove(0));
    app.groups.push(group);
    assert_disk_paths(
        &mut cache,
        &app,
        &["replacement.rs".into(), "renamed.rs".into()],
    );
    app.groups[0].tabs[0].editor.file_path = Some("inactive-rename.rs".into());
    assert_disk_paths(
        &mut cache,
        &app,
        &["inactive-rename.rs".into(), "renamed.rs".into()],
    );
    app.tabs.clear();
    assert_disk_paths(&mut cache, &app, &["inactive-rename.rs".into()]);
    app.groups.clear();
    assert_disk_paths(&mut cache, &app, &[]);
}

#[test]
fn workbench_problems_cache_tracks_documents_not_cursor_motion() {
    let mut app = App::test_new();
    app.tabs = vec![tab("first.rs"), tab("second.rs")];
    let target = app.problems_analysis_target();
    app.tabs[0].editor.cursor_col += 1;
    app.tabs[1].editor.scroll_offset += 10;
    app.active_tab = 1;
    assert!(app.problems_analysis_is_current(&target));

    app.tabs[0].editor.content_version += 1;
    assert!(!app.problems_analysis_is_current(&target));
    app.tabs[0].editor.content_version -= 1;
    app.tabs[1].editor.file_path = Some("renamed.rs".into());
    assert!(!app.problems_analysis_is_current(&target));
    app.tabs[1].editor.file_path = Some("second.rs".into());
    app.tabs.swap(0, 1);
    assert!(!app.problems_analysis_is_current(&target));
    app.tabs.swap(0, 1);
    app.active_group += 1;
    assert!(!app.problems_analysis_is_current(&target));
    app.active_group -= 1;
    assert!(app.problems_analysis_is_current(&target));

    // Reloading an identically named/length/versioned document cannot reuse
    // an outstanding scan from the closed editor.
    let mut replacement = tab("first.rs");
    replacement.editor.rope = ropey::Rope::from_str("// TODO: next!\n");
    assert_eq!(
        replacement.editor.rope.len_bytes(),
        app.tabs[0].editor.rope.len_bytes()
    );
    assert_eq!(
        replacement.editor.content_version,
        app.tabs[0].editor.content_version
    );
    app.tabs[0] = replacement;
    assert!(!app.problems_analysis_is_current(&target));
}

#[test]
fn workbench_problems_cache_handles_loading_untitled_and_large_documents() {
    let mut app = App::test_new();
    app.tabs = vec![
        Tab::new_file(Editor::new()),
        tab("large.rs"),
        tab("loading.rs"),
    ];
    app.tabs[1].editor.large_file_mode = true;
    app.tabs[2].editor.bg_loading = true;
    let target = app.problems_analysis_target();
    assert_eq!(target.files.len(), 1);
    app.tabs[1].editor.content_version += 1;
    app.tabs[2].editor.content_version += 1;
    assert!(app.problems_analysis_is_current(&target));
    app.tabs[0].editor.suggested_file_name = Some("Untitled.py".into());
    assert!(!app.problems_analysis_is_current(&target));
    let target = app.problems_analysis_target();
    app.tabs[2].editor.bg_loading = false;
    assert!(!app.problems_analysis_is_current(&target));
    let target = app.problems_analysis_target();
    app.tabs[1].editor.large_file_mode = false;
    assert!(!app.problems_analysis_is_current(&target));
    let target = app.problems_analysis_target();
    app.tabs.pop();
    assert!(!app.problems_analysis_is_current(&target));
    app.tabs.clear();
    assert!(app.problems_analysis_is_current(&app.problems_analysis_target()));
}

fn median(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_workbench_idle_inventory_and_problems_keys() {
    let mut app = App::test_new();
    app.groups.clear();
    app.explorer.root = "workspace".into();
    for count in [32, 256] {
        app.tabs = (0..count)
            .map(|index| {
                let directory = if index % 4 == 0 { "target" } else { "src" };
                tab(app
                    .explorer
                    .root
                    .join(format!("{directory}/module/submodule/file_{index}.rs")))
            })
            .collect();
        for watched in [false, true] {
            app.workspace_watcher_active = watched;
            let mut cache = PeriodicDiskPaths::default();
            cache.get(&app);
            let mut before = Vec::new();
            let mut after = Vec::new();
            for sample in 0..7 {
                for cached in [sample % 2 == 0, sample % 2 != 0] {
                    let start = Instant::now();
                    for _ in 0..1_000 {
                        if cached {
                            black_box(cache.get(black_box(&app)));
                        } else {
                            black_box(uncached_disk_paths(black_box(&app)));
                        }
                    }
                    if cached {
                        after.push(start.elapsed());
                    } else {
                        before.push(start.elapsed());
                    }
                }
            }
            eprintln!(
                "disk inventory, {count} tabs, watcher={watched}: {:?} -> {:?} per frame",
                median(&mut before) / 1_000,
                median(&mut after) / 1_000
            );
        }
        let target = app.problems_analysis_target();
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for cached in [sample % 2 == 0, sample % 2 != 0] {
                let start = Instant::now();
                for _ in 0..1_000 {
                    if cached {
                        black_box(black_box(&app).problems_analysis_is_current(black_box(&target)));
                    } else {
                        black_box(
                            black_box(&app).problems_analysis_target() == *black_box(&target),
                        );
                    }
                }
                if cached {
                    after.push(start.elapsed());
                } else {
                    before.push(start.elapsed());
                }
            }
        }
        eprintln!(
            "Problems cache key, {count} tabs: {:?} -> {:?} per redraw",
            median(&mut before) / 1_000,
            median(&mut after) / 1_000
        );
    }
}
