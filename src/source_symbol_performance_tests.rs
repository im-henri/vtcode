//! Opt-in measurements over a frozen copy of the actual vtcode source corpus.
use super::*;
use crate::{
    app::{SidebarView, Tab},
    editor::Editor,
};
use std::{
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};

fn digest(app: &App) -> u64 {
    let mut state = std::collections::hash_map::DefaultHasher::new();
    for item in &app.symbol_list_items {
        (
            &item.name,
            item.kind,
            item.line,
            item.tab_index,
            &item.file_name,
        )
            .hash(&mut state);
    }
    state.finish()
}

#[test]
#[ignore = "manual source-symbol latency measurement; requires VTCODE_OPTIMIZATION_CORPUS"]
fn benchmark_vtcode_source_symbols() {
    let root = PathBuf::from(
        std::env::var_os("VTCODE_OPTIMIZATION_CORPUS").expect("frozen source corpus"),
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let files: Vec<_> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| {
            let path = file["path"].as_str().unwrap();
            let text = std::fs::read_to_string(root.join(path)).unwrap();
            let expected = file["sha256"].as_str().unwrap();
            use sha2::{Digest, Sha256};
            assert_eq!(format!("{:x}", Sha256::digest(text.as_bytes())), expected);
            (path.to_owned(), text)
        })
        .collect();
    let reported = [
        "animation_config.rs",
        "animation_inline_scope_tests.rs",
        "animation_language_scope_tests.rs",
        "animation_multicursor_tests.rs",
        "animation_pacing.rs",
        "animation_preprocessor_scope_tests.rs",
        "animation_scope.rs",
        "animation_scope_blame_tests.rs",
        "animation_scope_checkpoints.rs",
        "animation_scope_edit_tests.rs",
        "animation_scope_edits.rs",
        "animation_scope_indentation.rs",
        "animation_scope_preprocessor.rs",
    ];
    let mut reports = Vec::new();
    for case in ["active-file", "reported-13", "all-source"] {
        let selected: Vec<_> = files
            .iter()
            .filter(|(path, _)| match case {
                "active-file" => path == "src/app.rs",
                "reported-13" => reported.iter().any(|name| path == &format!("src/{name}")),
                _ => true,
            })
            .collect();
        assert!(!selected.is_empty());
        let mut app = App::test_new();
        app.tabs = selected
            .iter()
            .map(|(path, text)| {
                let mut editor = Editor::test_with_text(text);
                editor.file_path = Some(PathBuf::from(path));
                Tab::new_file(editor)
            })
            .collect();
        app.active_tab = 0;
        app.symbol_list_workspace = true;
        app.symbol_list_active = true;
        let count = selected
            .iter()
            .map(|(_, text)| {
                text.lines()
                    .filter(|line| {
                        super::source_symbol(line).is_some_and(|(name, _)| !name.is_empty())
                    })
                    .count()
            })
            .sum::<usize>();
        let mut dispatch = Vec::new();
        let mut ready = Vec::new();
        let mut apply = Vec::new();
        let mut expected_digest = None;
        for _ in 0..9 {
            app.symbol_list_items.clear();
            app.symbol_list_filtered.clear();
            let started = Instant::now();
            app.build_symbol_list();
            dispatch.push(started.elapsed().as_nanos() as u64);
            let mut longest = Duration::ZERO;
            while app.symbol_list_items.len() != count {
                assert!(
                    started.elapsed() < Duration::from_secs(30),
                    "symbols did not complete"
                );
                let turn = Instant::now();
                app.poll_background_results();
                longest = longest.max(turn.elapsed());
                std::thread::yield_now();
            }
            ready.push(started.elapsed().as_nanos() as u64);
            apply.push(longest.as_nanos() as u64);
            let actual = digest(&app);
            if let Some(expected) = expected_digest {
                assert_eq!(actual, expected);
            } else {
                expected_digest = Some(actual);
            }
        }
        dispatch.sort_unstable();
        ready.sort_unstable();
        apply.sort_unstable();
        app.symbol_list_active = false;
        app.sidebar_view = SidebarView::Outline;
        let outline_count = selected[0]
            .1
            .lines()
            .filter(|line| super::source_symbol(line).is_some_and(|(name, _)| !name.is_empty()))
            .count();
        app.outline_items.clear();
        let started = Instant::now();
        app.update_outline();
        let outline_dispatch = started.elapsed();
        while app.outline_items.len() != outline_count {
            assert!(started.elapsed() < Duration::from_secs(30));
            app.poll_background_results();
            std::thread::yield_now();
        }
        let report = serde_json::json!({"case":case,"files":selected.len(),"bytes":selected.iter().map(|(_,s)|s.len()).sum::<usize>(),
            "symbols":count,"digest":format!("{:016x}",expected_digest.unwrap()),"samples":9,
            "dispatchMedianNs":dispatch[4],"readyMedianNs":ready[4],"longestApplyMedianNs":apply[4],
            "outlineDispatchNs":outline_dispatch.as_nanos() as u64,"outlineReadyNs":started.elapsed().as_nanos() as u64});
        eprintln!("{report}");
        reports.push(report);
    }
    if let Some(output) = std::env::var_os("VTCODE_OPTIMIZATION_SYMBOL_REPORT") {
        std::fs::write(
            output,
            serde_json::to_vec_pretty(
                &serde_json::json!({"corpusRevision":manifest["revision"],"cases":reports}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}
