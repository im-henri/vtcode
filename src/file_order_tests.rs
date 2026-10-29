use super::*;

#[test]
fn vscode_ordering_reference_fixtures_match_all_supported_comparers() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("../tests/fixtures/vscode-ordering.json")).unwrap();
    let strings = |values: &serde_json::Value| values.as_array().unwrap().iter()
        .map(|value| value.as_str().unwrap().to_owned()).collect::<Vec<_>>();
    for case in fixture["locales"].as_array().unwrap() {
        let locale = case["locale"].as_str().unwrap();
        let order = FileOrder::new(locale).unwrap();
        for (field, search) in [("names", false), ("searchNames", true)] {
            let mut actual = strings(&fixture["inputs"]["names"]);
            actual.sort_by(|a, b| if search { order.search_names(a, b) } else { order.names(a, b) });
            assert_eq!(actual, strings(&case[field]), "{locale}: {field}");
        }
        let mut actual = strings(&fixture["inputs"]["paths"]);
        actual.sort_by(|a, b| order.paths(&SearchPathKey::new(a, a.rsplit('/').next().unwrap()),
            &SearchPathKey::new(b, b.rsplit('/').next().unwrap())));
        assert_eq!(actual, strings(&case["paths"]), "{locale}: paths");
        for mode in ["default", "type"] {
            let mut entries = fixture["inputs"]["entries"].as_array().unwrap().clone();
            entries.sort_by(|a, b| {
                let (a_dir, b_dir) = (a["isDirectory"].as_bool().unwrap(), b["isDirectory"].as_bool().unwrap());
                b_dir.cmp(&a_dir).then_with(|| {
                    let (a, b) = (a["name"].as_str().unwrap(), b["name"].as_str().unwrap());
                    if mode == "type" && !a_dir { order.extensions(a, b) } else { order.names(a, b) }
                })
            });
            assert_eq!(entries.iter().map(|entry| entry["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>(),
                strings(&case["explorer"][mode]), "{locale}: Explorer {mode}");
        }
    }
}

#[test]
fn vscode_ordering_distinguishes_explorer_names_from_search_paths() {
    let order = FileOrder::new("en-US").unwrap();
    assert!(order.names("system_defines.h", "system.h").is_lt());
    assert!(order.names("system.h", "system.h.extra").is_lt());
    assert!(order.names("file1", "file01").is_lt());
    assert!(order.search_names("file1", "file01").is_gt());
    let compare = |a: &str, b: &str| order.paths(&SearchPathKey::new(a, a.rsplit('/').next().unwrap()),
        &SearchPathKey::new(b, b.rsplit('/').next().unwrap()));
    assert!(compare("z.h", "a/nested.h").is_lt());
    assert!(compare("a/z.h", "a/deep/a.h").is_lt());
    assert!(compare("folder10/a.h", "folder2/a.h").is_lt());
    assert!(order.names("folder10", "folder2").is_gt());
}

#[test]
fn vscode_ordering_explorer_refresh_and_locale_changes_rebuild_cached_order() {
    let root = tempfile::tempdir().unwrap();
    for name in ["system.h", "system_defines.h", "file10.h", "file2.h", "z.h", "õ.h"] {
        std::fs::write(root.path().join(name), "source").unwrap();
    }
    let mut explorer = crate::explorer::FileExplorer::new_deferred(root.path().to_owned());
    assert!(explorer.set_sort_locale("en-US"));
    explorer.refresh();
    let names = |explorer: &crate::explorer::FileExplorer| explorer.entries.iter().map(|row| row.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&explorer), ["file2.h", "file10.h", "õ.h", "system_defines.h", "system.h", "z.h"]);
    assert!(explorer.set_sort_locale("et-EE"));
    explorer.refresh();
    assert_eq!(names(&explorer), ["file2.h", "file10.h", "system_defines.h", "system.h", "z.h", "õ.h"]);
}

#[test]
fn vscode_ordering_streamed_app_results_preserve_selected_match_and_scroll_anchor() {
    use crate::{app::{App, GlobalSearchResult}, background::BackgroundResult};
    let result = |file: &str, line| GlobalSearchResult { file_path: file.into(), line_num: line,
        line_text: "needle".into(), col_start: 0, col_end: 6, context_before: None, context_after: None };
    let mut app = App::test_new();
    app.global_search_results = vec![result("m.h", 2), result("m.h", 3), result("z.h", 1)].into();
    app.global_search_results.set_collapsed(Path::new("z.h"), true);
    app.global_search_selected = 1;
    app.global_search_scroll = 2;
    app.background.sender.send(BackgroundResult::GlobalSearchResults {
        results: vec![result("a.h", 1), result("m.h", 1)], done: true,
        generation: app.search_generation, truncated: false,
    }).unwrap();
    app.poll_background_results();
    let selected = &app.global_search_results[app.global_search_selected];
    assert_eq!((&*selected.file_path, selected.line_num), (Path::new("m.h"), 3));
    assert_eq!(app.global_search_selected, 3);
    assert_eq!(app.global_search_scroll, 5);
    assert!(app.global_search_results.is_collapsed(Path::new("z.h")));
    assert_eq!(app.global_search_results.file_count(), 3);
}
