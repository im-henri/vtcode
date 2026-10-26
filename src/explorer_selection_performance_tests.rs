use super::*;
use crate::explorer::{FileEntry, GitStatus};
use std::time::{Duration, Instant};

fn fixture(count: usize, duplicate: bool) -> App {
    let mut app = App::test_new();
    app.explorer.entries = (0..count).map(|index| {
        let name = format!("file-{}界.rs", if duplicate { index % 37 } else { index });
        FileEntry {
            path: PathBuf::from("workspace").join(&name), name,
            is_dir: index % 11 == 0, depth: 0, expanded: false,
            git_status: GitStatus::Unmodified, git_status_summary: Default::default(),
        }
    }).collect();
    app
}

fn reference(app: &mut App, index: usize, additive: bool) {
    if index >= app.explorer.entries.len() { return; }
    let anchor = app.explorer_selection_anchor.unwrap_or(app.explorer.selected);
    if !additive { app.explorer_multi_selected.clear(); }
    for row in anchor.min(index)..=anchor.max(index) {
        if let Some(entry) = app.explorer.entries.get(row) {
            if !app.explorer_multi_selected.iter().any(|path| path == &entry.path) {
                app.explorer_multi_selected.push(entry.path.clone());
            }
        }
    }
    app.explorer.selected = index;
}

#[test]
fn explorer_range_selection_preserves_order_duplicates_and_stale_anchors() {
    for count in [0, 1, 31, 32, 33, 150] {
        for duplicate in [false, true] {
            let mut app = fixture(count, duplicate);
            for anchor in [None, Some(0), Some(17), Some(count + 5)] {
                for index in [0, 12, 32, count.saturating_sub(1), count + 1] {
                    for additive in [false, true] {
                        app.explorer_selection_anchor = anchor;
                        app.explorer.selected = 3;
                        let initial: Vec<_> = ["old", "old", "workspace/file-17界.rs"]
                            .into_iter().map(PathBuf::from).collect();
                        app.explorer_multi_selected = initial.clone().into();
                        reference(&mut app, index, additive);
                        let expected = app.explorer_multi_selected.clone();
                        let expected_index = app.explorer.selected;
                        app.explorer_multi_selected = initial.into();
                        app.explorer.selected = 3;
                        app.range_select_explorer(index, additive);
                        assert_eq!(app.explorer_multi_selected, expected,
                            "count={count} duplicate={duplicate} anchor={anchor:?} index={index} additive={additive}");
                        assert_eq!(app.explorer.selected, expected_index);
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "comparative release Explorer range-selection benchmark"]
fn benchmark_explorer_range_selection() {
    let mut app = fixture(10_000, false);
    app.explorer_selection_anchor = Some(0);
    let mut timings = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            app.explorer_multi_selected.clear();
            let started = Instant::now();
            if optimized { app.range_select_explorer(9_999, false); }
            else { reference(&mut app, 9_999, false); }
            timings[usize::from(optimized)].push(started.elapsed());
            assert_eq!(app.explorer_multi_selected,
                app.explorer.entries.iter().map(|entry| entry.path.clone()).collect::<Vec<_>>());
        }
    }
    let medians: Vec<Duration> = timings.iter_mut().map(|samples| {
        samples.sort(); samples[samples.len() / 2]
    }).collect();
    eprintln!("10,000-file range selection: reference={:?}, optimized={:?}", medians[0], medians[1]);
}
