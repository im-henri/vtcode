use super::*;
use crate::{
    app::{EditorGroup, Tab},
    editor::Editor,
};

fn tab(path: impl Into<PathBuf>) -> Tab {
    let mut editor = Editor::new();
    editor.file_path = Some(path.into());
    Tab::new_file(editor)
}

fn fixture(paths: &[&str]) -> App {
    let mut app = App::test_new();
    app.tabs = paths.iter().map(|path| tab(*path)).collect();
    app
}

fn label(app: &App, tab: &Tab) -> String {
    super::super::tab_display_name(app, 0, tab).into_owned()
}

fn assert_unique_paths(app: &App) {
    let mut labels = FxHashMap::default();
    for tab in all_tabs(app) {
        let Some((_, parent)) = parent(tab) else {
            continue;
        };
        let title = label(app, tab);
        if let Some(previous) = labels.insert(title.clone(), parent) {
            assert!(
                common_tail(previous, parent).1,
                "different paths share {title:?}: {previous:?}, {parent:?}"
            );
        }
    }
}

#[test]
fn shared_src_directories_show_the_distinguishing_core() {
    let app = fixture(&[
        "cores/arm_cache/src/entry.cps",
        "cores/axi_rv32/src/entry.cps",
    ]);
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        label(&app, &app.tabs[0]),
        format!("entry.cps (…{sep}arm_cache{sep}…)")
    );
    assert_eq!(
        label(&app, &app.tabs[1]),
        format!("entry.cps (…{sep}axi_rv32{sep}…)")
    );
}

#[test]
fn shortest_context_handles_many_files_unequal_depths_and_shared_ancestors() {
    for paths in [
        vec!["a/src/entry.cps", "b/src/entry.cps", "a/test/entry.cps"],
        vec![
            "a/lib/src/entry.cps",
            "b/lib/src/entry.cps",
            "a/test/src/entry.cps",
        ],
        vec![
            "src/entry.cps",
            "a/src/entry.cps",
            "b/a/src/entry.cps",
            "entry.cps",
        ],
        vec![
            "one/same/src/entry.cps",
            "two/same/src/entry.cps",
            "two/other/src/entry.cps",
        ],
        vec![
            "界/同じ/src/entry.cps",
            "é/同じ/src/entry.cps",
            "é/同じ/src/entry.cps",
        ],
        vec![
            "a/…/src/entry.cps",
            "a/%E2%80%A6/src/entry.cps",
            "a/normal/src/entry.cps",
        ],
        vec![
            "dir\n/src/entry.cps",
            "dir%0A/src/entry.cps",
            "normal/src/entry.cps",
        ],
        vec!["Foo/src/entry.cps", "foo/src/entry.cps"],
        vec![
            "a/../src/entry.cps",
            "b/../src/entry.cps",
            "../src/entry.cps",
        ],
    ] {
        assert_unique_paths(&fixture(&paths));
    }
}

#[test]
fn identical_views_and_lexical_dot_aliases_keep_the_same_title() {
    let app = fixture(&["a/src/entry.cps", "a/./src/entry.cps", "a/src//entry.cps"]);
    for tab in &app.tabs {
        assert_eq!(label(&app, tab), "entry.cps");
    }
    let app = fixture(&["entry.cps", "./entry.cps", "src/entry.cps"]);
    assert_eq!(label(&app, &app.tabs[0]), "entry.cps (.)");
    assert_eq!(label(&app, &app.tabs[1]), "entry.cps (.)");
    assert_unique_paths(&app);
}

#[test]
fn contexts_follow_rename_close_reorder_and_split_group_membership() {
    let mut app = fixture(&["cores/arm/src/entry.cps", "cores/rv/src/entry.cps"]);
    app.tabs.swap(0, 1);
    assert!(label(&app, &app.tabs[0]).contains("rv"));
    let other = app.tabs.remove(1);
    app.groups = vec![EditorGroup::default(), EditorGroup::default()];
    app.groups[1].tabs.push(other);
    assert!(label(&app, &app.tabs[0]).contains("rv"));
    assert!(label(&app, &app.groups[1].tabs[0]).contains("arm"));
    app.groups[1].tabs[0].editor.file_path = Some("cores/arm/src/other.cps".into());
    assert_eq!(label(&app, &app.tabs[0]), "entry.cps");
    app.groups[1].tabs[0].editor.file_path = Some("cores/arm/src/entry.cps".into());
    app.groups[1].tabs.clear();
    assert_eq!(label(&app, &app.tabs[0]), "entry.cps");
}

#[test]
fn indexed_conflicts_include_distant_tabs_but_storage_follows_the_viewport() {
    let mut app = App::test_new();
    app.tabs = (0..10_000)
        .map(|index| tab(format!("cores/core-{index}/common/deeper/src/entry.cps")))
        .collect();
    let index = TabNameDuplicates::for_viewport(&app, 80).unwrap();
    assert_eq!(index.name_count(), 1);
    assert!(
        index.node_count() < 100,
        "unseen paths must not grow the trie"
    );
    for tab in app.tabs.iter().take(20) {
        assert_eq!(index.context(tab).unwrap(), context(&app, tab));
    }
    let first = index.context(&app.tabs[0]).unwrap().unwrap();
    assert!(first.contains("core-0"));
    assert!(!first.contains("common"));
}

#[test]
fn indexed_conflicts_also_work_when_every_conflicting_path_is_visible() {
    for paths in [
        ["core-a/src/entry.cps", "core-b/src/entry.cps"],
        ["one/entry.cps", "two/entry.cps"],
        ["entry.cps", "src/entry.cps"],
    ] {
        let mut app = fixture(&paths);
        app.tabs
            .extend((0..100).map(|index| tab(format!("unrelated/file-{index}.txt"))));
        let index = TabNameDuplicates::for_viewport(&app, 80).unwrap();
        for tab in app.tabs.iter().take(2) {
            assert_eq!(index.context(tab).unwrap(), context(&app, tab));
            assert!(index.context(tab).unwrap().is_some());
        }
    }
}

#[test]
fn cached_hints_reuse_redraws_but_detect_path_and_kind_changes() {
    let mut app = fixture(&["a/src/entry.cps", "b/src/entry.cps"]);
    app.tabs
        .extend((0..100).map(|index| tab(format!("other/file-{index}.txt"))));
    for _ in 0..10 {
        let index = TabNameDuplicates::for_viewport(&app, 100).unwrap();
        assert_eq!(
            index.context(&app.tabs[0]).unwrap(),
            context(&app, &app.tabs[0])
        );
    }
    assert_eq!(app.tab_name_cache.borrow().builds, 1);
    app.tabs.swap(0, 1);
    let index = TabNameDuplicates::for_viewport(&app, 100).unwrap();
    assert!(index.context(&app.tabs[0]).unwrap().unwrap().contains("b"));
    assert_eq!(
        app.tab_name_cache.borrow().builds,
        1,
        "reordering the same candidates can reuse their hints"
    );
    app.tabs[1].kind = TabKind::MarkdownPreview;
    let index = TabNameDuplicates::for_viewport(&app, 100).unwrap();
    assert_eq!(index.context(&app.tabs[0]), Some(None));
    app.tabs[1].kind = TabKind::File;
    app.tabs[1].editor.file_path = Some("a/src/renamed.cps".into());
    let index = TabNameDuplicates::for_viewport(&app, 100).unwrap();
    assert_eq!(index.context(&app.tabs[0]), Some(None));
    app.tabs[1].editor.file_path = Some("a/src/entry.cps".into());
    let index = TabNameDuplicates::for_viewport(&app, 100).unwrap();
    assert!(index.context(&app.tabs[0]).unwrap().is_some());
    app.tabs.remove(1);
    let index = TabNameDuplicates::for_viewport(&app, 100).unwrap();
    assert_eq!(index.context(&app.tabs[0]), Some(None));
}

#[test]
fn recent_viewport_cache_is_bounded_and_keeps_multiple_panes_warm() {
    let mut app = App::test_new();
    app.tabs = (0..200)
        .map(|index| tab(format!("core-{index}/src/entry.cps")))
        .collect();
    for scroll in 0..25 {
        app.tab_scroll_offset = scroll;
        TabNameDuplicates::for_viewport(&app, 80).unwrap();
        assert!(app.tab_name_cache.borrow().entries.len() <= 16);
    }
    let builds = app.tab_name_cache.borrow().builds;
    for _ in 0..4 {
        for scroll in [24, 23, 22, 21] {
            app.tab_scroll_offset = scroll;
            TabNameDuplicates::for_viewport(&app, 80).unwrap();
        }
    }
    assert_eq!(app.tab_name_cache.borrow().builds, builds);
}

#[test]
fn trie_matches_pairwise_oracle_for_adversarial_paths_and_viewports() {
    let mut state = 0x67ab_u64;
    for round in 0..16 {
        let mut app = App::test_new();
        for _ in 0..100 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let depth = (state as usize % 6) + 1;
            let mut path = PathBuf::new();
            for _ in 0..depth {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                path.push(["src", "lib", "same", "a", "b", "c"][state as usize % 6]);
            }
            path.push("entry.cps");
            app.tabs.push(tab(path));
        }
        app.tab_scroll_offset = round * 5;
        let index = TabNameDuplicates::for_viewport(&app, 80).unwrap();
        for tab in app.tabs.iter().skip(app.tab_scroll_offset).take(22) {
            assert_eq!(index.context(tab).unwrap(), context(&app, tab));
        }
        assert_unique_paths(&app);
    }
}

#[test]
#[ignore = "manual linear-time tab path disambiguation probe"]
fn benchmark_visible_path_disambiguation() {
    for shape in ["unique", "duplicate", "deep"] {
        for count in [100, 1_000, 10_000] {
            let mut app = App::test_new();
            app.tabs = (0..count)
                .map(|index| {
                    let tail = if shape == "deep" {
                        "common/".repeat(12)
                    } else {
                        "src/".into()
                    };
                    let name = if shape == "unique" {
                        format!("file-{index}.cps")
                    } else {
                        "entry.cps".into()
                    };
                    tab(format!("cores/core-{index}/{tail}{name}"))
                })
                .collect();
            let cold = std::time::Instant::now();
            std::hint::black_box(TabNameDuplicates::for_viewport(&app, 180));
            let cold = cold.elapsed();
            let start = std::time::Instant::now();
            for _ in 0..25 {
                let names =
                    TabNameDuplicates::for_viewport(std::hint::black_box(&app), 180).unwrap();
                for tab in app.tabs.iter().take(5) {
                    std::hint::black_box(names.context(tab));
                }
            }
            eprintln!(
                "path hints {shape}, {count} tabs: {:?} cold, {:?} cached per viewport",
                cold,
                start.elapsed() / 25
            );
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_drives_unc_roots_and_relative_paths_remain_distinct() {
    let root_files = fixture(&[r"C:\entry.cps", r"D:\entry.cps"]);
    assert_eq!(label(&root_files, &root_files.tabs[0]), r"entry.cps (C:\)");
    assert_eq!(label(&root_files, &root_files.tabs[1]), r"entry.cps (D:\)");
    for paths in [
        vec!["C:/workspace/src/entry.cps", "D:/workspace/src/entry.cps"],
        vec![
            r"\\server-a\share\src\entry.cps",
            r"\\server-b\share\src\entry.cps",
        ],
        vec![
            r"\\server\share-a\src\entry.cps",
            r"\\server\share-b\src\entry.cps",
        ],
        vec![r"C:\src\entry.cps", r"src\entry.cps", r"\src\entry.cps"],
        vec![r"C:\entry.cps", r"D:\entry.cps", "entry.cps"],
        vec![r"C:src\entry.cps", r"C:\src\entry.cps"],
        vec![r"C:folder\src\entry.cps", r"C:\folder\src\entry.cps"],
    ] {
        assert_unique_paths(&fixture(&paths));
    }
}

#[cfg(windows)]
#[test]
fn invalid_unicode_directories_are_not_lossily_merged() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let mut app = App::test_new();
    for code in [0xD800, 0xD801] {
        let mut path = PathBuf::from("base");
        path.push(OsString::from_wide(&[code]));
        path.push("src/entry.cps");
        app.tabs.push(tab(path));
    }
    assert_unique_paths(&app);
    assert!(label(&app, &app.tabs[0]).contains("\\x"));
}
