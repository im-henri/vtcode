use super::*;
use crate::{
    app::{App, EditorGroup, SplitDirection},
    editor::Editor,
};

fn tab(index: usize) -> Tab {
    let mut editor = Editor::test_with_text("value = 1\n");
    editor.file_path = Some(format!("file-{index}.rs").into());
    Tab::new_file(editor)
}

#[test]
fn compaction_keeps_selection_and_recency_attached_to_the_same_editors() {
    for mask in 0..64 {
        for selected in 0..6 {
            let mut tabs: Vec<_> = (0..6).map(tab).collect();
            let ids: Vec<_> = tabs.iter().map(|tab| tab.editor.navigation_id).collect();
            let old_mru = [4, 1, 5, 0, 2, 3];
            let mut mru = old_mru.to_vec();
            let mut active = selected;
            retain(&mut tabs, &mut active, &mut mru, |index, _| {
                mask & (1 << index) != 0
            });
            let expected: Vec<_> = old_mru
                .into_iter()
                .filter(|i| mask & (1 << i) != 0)
                .map(|i| ids[i])
                .collect();
            assert_eq!(
                mru.iter()
                    .map(|&i| tabs[i].editor.navigation_id)
                    .collect::<Vec<_>>(),
                expected
            );
            if tabs.is_empty() {
                assert_eq!(active, 0);
            } else {
                let expected = if mask & (1 << selected) != 0 {
                    ids[selected]
                } else {
                    expected[0]
                };
                assert_eq!(tabs[active].editor.navigation_id, expected);
            }
        }
    }
}

#[test]
fn activation_history_is_complete_unique_and_starts_with_the_current_tab() {
    let mut mru = vec![99, 4, 1, 4, 0, 1];
    complete_mru(&mut mru, 2, 6);
    assert_eq!(mru, [2, 4, 1, 0, 3, 5]);
    complete_mru(&mut mru, 0, 6);
    assert_eq!(mru, [0, 2, 4, 1, 3, 5]);
    complete_mru(&mut mru, 0, 0);
    assert!(mru.is_empty());
}

#[test]
fn pane_switches_preserve_each_panes_own_recency_without_reallocating() {
    let mut app = App::test_new();
    app.tabs = (0..3).map(tab).collect();
    app.active_tab = 2;
    app.tab_mru_order = vec![2, 0, 1];
    let original = app.tab_mru_order.as_ptr();
    let mut group = EditorGroup::new();
    group.tabs = (3..5).map(tab).collect();
    group.active_tab = 1;
    group.tab_mru_order = vec![1, 0];
    let other = group.tab_mru_order.as_ptr();
    app.groups = vec![EditorGroup::new(), group];
    for _ in 0..3 {
        app.switch_group(1);
        assert_eq!(app.tab_mru_order, [1, 0]);
        assert_eq!(app.tab_mru_order.as_ptr(), other);
        app.switch_group(0);
        assert_eq!(app.tab_mru_order, [2, 0, 1]);
        assert_eq!(app.tab_mru_order.as_ptr(), original);
    }
    app.force_close_tab(2);
    assert_eq!(app.editor().unwrap().file_name(), "file-0.rs");
}

#[test]
fn splitting_moving_reordering_and_merging_tabs_preserve_history() {
    let mut app = App::test_new();
    app.tabs = (0..5).map(tab).collect();
    app.active_tab = 3;
    app.tab_mru_order = vec![3, 1, 4, 0, 2];
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    assert_eq!(app.tab_mru_order, [0]);
    assert_eq!(app.groups[0].tab_mru_order, [2, 3, 0, 1]);
    assert!(app.move_tab_between_groups(0, 0, 1));
    assert_eq!(app.groups[0].tab_mru_order, [1, 2, 0]);
    app.touch_tab_mru(1);
    app.active_tab = 1;
    app.execute_palette_command("move_tab_left");
    assert_eq!(app.tab_mru_order, [0, 1]);
    app.close_all_splits();
    assert_eq!(app.tab_mru_order, [0, 1, 3, 4, 2]);
    assert_eq!(app.editor().unwrap().file_name(), "file-0.rs");
}

#[test]
fn closing_inactive_tabs_preserves_active_search_results_and_diff() {
    let mut app = App::test_new();
    app.tabs = (0..4).map(tab).collect();
    app.active_tab = 2;
    let id = app.tabs[2].editor.navigation_id;
    app.tab_mru_order = vec![2, 3, 0, 1];
    app.search_active = true;
    app.search_matches = vec![(0, 0, 5)];
    app.diff_mode = crate::app::DiffMode::Inline;
    app.close_tabs_matching(|i| i != 2);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().navigation_id, id);
    assert_eq!(app.search_matches, [(0, 0, 5)]);
    assert!(app.search_active);
    assert!(app.diff_mode == crate::app::DiffMode::Inline);
    assert_eq!(app.tab_mru_order, [0]);
}

#[test]
fn preview_replacement_clears_search_for_the_previous_editor() {
    let mut app = App::test_new();
    app.enable_preview_tabs = true;
    let mut preview = tab(0);
    preview.preview = true;
    app.tabs = vec![preview];
    app.search_matches = vec![(0, 0, 5)];
    app.open_file_preview(std::path::Path::new("file-next.rs"));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().file_name(), "file-next.rs");
    assert!(app.search_matches.is_empty());
}

#[test]
fn palette_bulk_closes_keep_pinned_and_unsaved_tabs_and_the_selected_editor() {
    for command in ["close_others", "close_left", "close_right"] {
        let mut app = App::test_new();
        app.tabs = (0..8).map(tab).collect();
        app.active_tab = 3;
        app.tabs[0].editor.modified = true;
        app.tabs[7].editor.modified = true;
        app.tabs[1].pinned = true;
        app.tabs[5].pinned = true;
        app.execute_palette_command(command);
        assert_eq!(app.editor().unwrap().file_name(), "file-3.rs");
        for name in ["file-0.rs", "file-1.rs", "file-5.rs", "file-7.rs"] {
            assert!(app.tabs.iter().any(|tab| tab.editor.file_name() == name));
        }
        assert_eq!(
            app.tabs.len(),
            if command == "close_others" {
                5
            } else if command == "close_left" {
                7
            } else {
                6
            }
        );
        assert!(!app.confirm_close_active);
    }
}

#[test]
fn bulk_close_prompts_for_the_original_dirty_editor_after_compaction() {
    let mut app = App::test_new();
    app.tabs = (0..8).map(tab).collect();
    app.active_tab = 7;
    app.tabs[3].editor.modified = true;
    app.tabs[5].pinned = true;
    app.close_tabs_matching(|_| true);
    assert!(app.confirm_close_active);
    assert_eq!(
        app.tabs[app.confirm_close_tab].editor.file_name(),
        "file-3.rs"
    );
    assert_eq!(app.editor().unwrap().file_name(), "file-7.rs");
    assert_eq!(
        app.tabs
            .iter()
            .map(|tab| tab.editor.file_name())
            .collect::<Vec<_>>(),
        [
            "file-0.rs",
            "file-1.rs",
            "file-2.rs",
            "file-3.rs",
            "file-5.rs",
            "file-7.rs"
        ]
    );
}

#[test]
#[ignore = "manual optimized tab history benchmark"]
fn benchmark_tab_history_completion() {
    for count in [1_000, 10_000] {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for _ in 0..9 {
            let mut mru: Vec<_> = (0..count).rev().collect();
            let start = std::time::Instant::now();
            for i in 0..count {
                if !mru.contains(&i) {
                    mru.push(i);
                }
            }
            mru.retain(|&i| i < count);
            std::hint::black_box(&mru);
            before.push(start.elapsed());
            let start = std::time::Instant::now();
            complete_mru(&mut mru, count - 1, count);
            std::hint::black_box(&mru);
            after.push(start.elapsed());
        }
        before.sort_unstable();
        after.sort_unstable();
        eprintln!(
            "tab history {count}: before {:?}, after {:?}",
            before[4], after[4]
        );
    }
}
