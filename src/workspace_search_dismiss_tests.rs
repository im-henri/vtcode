use super::*;
use crate::{
    app::{ActivePane, Tab},
    background::BackgroundResult,
    editor::Editor,
    search_results::SearchRow,
    text_input::TextInputId,
    ui_search_controls::{SearchControl, SearchScope},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::sync::Arc;

fn result(file: &str, line_num: usize) -> GlobalSearchResult {
    GlobalSearchResult {
        file_path: file.into(),
        line_num,
        line_text: "needle 界".into(),
        col_start: 0,
        col_end: 6,
        context_before: None,
        context_after: None,
    }
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn dismissals_are_temporary_for_local_and_remote_searches_and_do_not_edit_files() {
    let root = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt"] {
        std::fs::write(root.path().join(name), "needle one\nneedle two\n").unwrap();
    }
    for remote in [false, true] {
        let workspace = remote.then(|| {
            crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
                root.path(),
                Arc::default(),
                Arc::default(),
            ))
        });
        let mut app = App::new_deferred(workspace.as_ref().map_or_else(
            || root.path().into(),
            |workspace| workspace.view_root.clone(),
        ));
        app.source_control_enabled = false;
        if let Some(workspace) = &workspace {
            workspace.client.set_wake(app.background.wake_handle());
        }
        app.remote_workspace = workspace;
        app.global_search_query = "needle".into();
        app.search_include_pattern = "*.txt".into();
        app.update_global_search();
        app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert!(
            app.global_search_error.is_none(),
            "{:?}",
            app.global_search_error
        );
        assert_eq!(app.global_search_results.len(), 4);
        let generation = app.search_generation;
        app.dismiss_global_search_result(SearchRow::Result(0));
        let last = app.global_search_results.len() - 1;
        app.dismiss_global_search_result(SearchRow::Header(last));
        assert_eq!(app.global_search_results.len(), 1);
        assert_eq!(app.global_search_results.file_count(), 1);
        assert_eq!(app.search_generation, generation);
        assert_eq!(app.global_search_query, "needle");
        assert_eq!(app.search_include_pattern, "*.txt");
        assert!(app.search_exclude_pattern.is_empty());
        for name in ["a.txt", "b.txt"] {
            assert_eq!(
                std::fs::read_to_string(root.path().join(name)).unwrap(),
                "needle one\nneedle two\n"
            );
        }
        crate::ui_search_controls::activate(
            &mut app,
            SearchScope::Workspace,
            SearchControl::Refresh,
        );
        app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert!(
            app.global_search_error.is_none(),
            "{:?}",
            app.global_search_error
        );
        assert_eq!(app.global_search_results.len(), 4);
        assert_eq!(app.global_search_results.file_count(), 2);
    }
}

#[test]
fn dismissed_streamed_matches_and_files_stay_hidden_without_disturbing_selection() {
    let mut app = App::test_new();
    app.search_generation = 7;
    app.bg_global_search_running = true;
    app.global_search_results = vec![result("a", 1), result("a", 2), result("b", 1)].into();
    app.global_search_selected = 1;
    app.dismiss_global_search_result(SearchRow::Result(0));
    app.dismiss_global_search_result(SearchRow::Header(1));
    app.background
        .sender
        .send(BackgroundResult::GlobalSearchResults {
            results: vec![
                result("a", 1),
                result("a", 3),
                result("b", 2),
                result("c", 1),
            ],
            generation: 7,
            done: true,
            truncated: false,
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(app.global_search_results.len(), 3);
    assert_eq!(app.global_search_results.file_count(), 2);
    assert_eq!(
        app.global_search_results[app.global_search_selected].line_num,
        2
    );
    assert_eq!(
        app.global_search_results[app.global_search_selected].file_path,
        Path::new("a")
    );
    assert_eq!(app.global_search_results.file_matches(Path::new("b")), 0);
    assert_eq!(app.global_search_results.file_matches(Path::new("a")), 2);
    assert!(!app.bg_global_search_running);
}

#[test]
fn dismiss_buttons_match_scrolled_rows_and_never_overlap_sidebar_seams_or_scrollbars() {
    for right in [false, true] {
        for width in [12, 24, 60] {
            for header in [false, true] {
                let mut app = App::test_new();
                app.focus_global_search();
                app.sidebar_right = right;
                app.global_search_query = "needle".into();
                app.global_search_results = (0..60)
                    .flat_map(|file| {
                        (1..=2).map(move |line| result(&format!("src/file{file:03}界.rs"), line))
                    })
                    .collect();
                let area = Rect::new(5, 3, width, 22);
                app.explorer_area = Some(area);
                app.global_search_scroll = 17;
                let row = app
                    .global_search_results
                    .visible_rows(18..28)
                    .find(|(_, row)| {
                        matches!(
                            (row, header),
                            (SearchRow::Header(_), true) | (SearchRow::Result(_), false)
                        )
                    })
                    .unwrap();
                let y = crate::ui_search_panel::search_results_start_y(&app, area)
                    + (row.0 - 17) as u16;
                app.mouse_col = area.x + 2;
                app.mouse_row = y;
                let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
                terminal
                    .draw(|frame| crate::ui_search_panel::draw_search_panel(frame, &mut app, area))
                    .unwrap();
                let button = app
                    .search_control_regions
                    .iter()
                    .find(|button| {
                        button.area.y == y
                            && matches!(button.control, SearchControl::DismissResult(_, _))
                    })
                    .copied()
                    .unwrap();
                assert_eq!(
                    button.control,
                    SearchControl::DismissResult(row.1, app.global_search_results.revision())
                );
                assert_eq!(button.area.intersection(area), button.area);
                assert!(button.area.x > area.x);
                assert!(button.area.right() < area.right() - u16::from(!right));
                assert_eq!(
                    terminal.backend().buffer()[(button.area.x + 1, y)].symbol(),
                    "×"
                );
                let selected = app.global_search_results[app.global_search_selected].clone();
                let old_count = app.global_search_results.len();
                let event = MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: button.area.x + 1,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                };
                crate::mouse::handle_mouse(&mut app, event);
                assert_eq!(
                    app.global_search_results.len(),
                    old_count - if header { 2 } else { 1 }
                );
                assert_eq!(app.global_search_scroll, 17);
                assert_eq!(
                    app.global_search_results[app.global_search_selected].file_path,
                    selected.file_path
                );
                assert!(!app.global_search_active);
                // A streamed mutation or prior click makes old rendered row
                // targets obsolete. Repeated input must not dismiss a neighbor.
                crate::mouse::handle_mouse(&mut app, event);
                assert_eq!(
                    app.global_search_results.len(),
                    old_count - if header { 2 } else { 1 }
                );
            }
        }
    }
}

#[test]
fn delete_dismisses_only_focused_search_results_and_keeps_text_editing_and_documents_intact() {
    let root = tempfile::tempdir().unwrap();
    for file in ["a", "b"] {
        std::fs::write(root.path().join(file), "needle one\nneedle two\n").unwrap();
    }
    let mut app = App::new_deferred(root.path().into());
    app.source_control_enabled = false;
    app.tabs = vec![Tab::new_file(Editor::test_with_text("keep this document"))];
    let document = app.tabs[0].editor.navigation_id;
    app.focus_global_search();
    app.global_search_query = "needle".into();
    app.global_search_results = vec![result("a", 1), result("a", 2), result("b", 1)]
        .into_iter()
        .map(|mut result| {
            result.file_path = root.path().join(result.file_path);
            result
        })
        .collect::<Vec<_>>()
        .into();
    app.global_search_results.set_all_collapsed(true);
    app.global_search_active = false;
    key(&mut app, KeyCode::Delete);
    assert_eq!(
        app.global_search_results.len(),
        3,
        "no invisible match is focused"
    );
    app.global_search_results.set_all_collapsed(false);
    app.focus_text_input(TextInputId::GlobalSearch(3));
    app.search_focus_field = 3;
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focused_text_input_id(), None);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.global_search_selected, 1);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().all(|tab| !tab.editor.bg_loading)
    });
    key(&mut app, KeyCode::Delete);
    assert_eq!(app.global_search_results.len(), 2);
    assert_eq!(app.global_search_results[1].file_path, root.path().join("b"));
    key(&mut app, KeyCode::Delete);
    key(&mut app, KeyCode::Delete);
    assert!(app.global_search_results.is_empty());
    assert_eq!(app.global_search_scroll, 0);
    assert_eq!(app.global_search_selected, 0);
    assert_eq!(app.global_search_query, "needle");
    key(&mut app, KeyCode::Tab);
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::GlobalSearch(0))
    );
    app.global_search_field_cursors[0] = 0;
    key(&mut app, KeyCode::Delete);
    assert_eq!(app.global_search_query, "eedle");
    let editor = &app.tabs.iter().find(|tab| tab.editor.navigation_id == document).unwrap().editor;
    assert_eq!(editor.rope.to_string(), "keep this document");
    assert!(!editor.modified);
}
