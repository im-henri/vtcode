use crate::{
    app::{ActivePane, App, GlobalSearchResult},
    settings::Settings,
    text_input::TextInputId,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use std::time::Duration;

#[path = "workspace_search_focus_tests.rs"]
mod focus_tests;

fn fixture(preview: bool) -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(
            root.path().join(name),
            "first needle\n界 needle tail\nlast needle\n",
        )
        .unwrap();
    }
    let mut app = App::new_deferred(root.path().into());
    app.source_control_enabled = false;
    app.apply_settings(&Settings {
        enable_preview_tabs: Some(preview),
        ..Default::default()
    });
    app.focus_global_search();
    app.global_search_query = "needle".into();
    app.global_search_results = [
        ("a.txt", 1, 6),
        ("a.txt", 2, 2),
        ("b.txt", 3, 5),
        ("c.txt", 1, 6),
    ]
    .into_iter()
    .map(|(file, line_num, col_start)| GlobalSearchResult {
        file_path: root.path().join(file),
        line_num,
        line_text: "needle".into(),
        col_start,
        col_end: col_start + 6,
        context_before: None,
        context_after: None,
    })
    .collect::<Vec<_>>()
    .into();
    app.explorer_area = Some(Rect::new(0, 0, 36, 10));
    (root, app)
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn wait_for_files(app: &mut App) {
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().all(|tab| !tab.editor.bg_loading)
    });
}

fn assert_selected_match(app: &App, selected: usize, preview: bool) {
    assert_eq!(app.global_search_selected, selected);
    assert_eq!(app.global_search_query, "needle");
    assert_eq!(app.active_pane, ActivePane::Explorer);
    assert!(!app.global_search_active);
    assert_eq!(app.focused_text_input_id(), None);
    let result = &app.global_search_results[selected];
    let editor = app.editor().unwrap();
    assert_eq!(editor.file_path.as_ref(), Some(&result.file_path));
    assert_eq!(
        (editor.cursor_line, editor.cursor_col),
        (result.line_num - 1, result.col_start)
    );
    assert_eq!(app.tabs[app.active_tab].preview, preview);
    let row = app.global_search_results.result_row(selected).unwrap();
    assert!((app.global_search_scroll..app.global_search_scroll + 2).contains(&row));
}

#[test]
fn arrows_open_each_match_keep_search_focus_and_honor_the_preview_setting() {
    for preview in [false, true] {
        let (_root, mut app) = fixture(preview);
        app.preview_global_search_result();
        wait_for_files(&mut app);
        assert_selected_match(&app, 0, preview);
        for (code, selected) in [
            (KeyCode::Down, 1),
            (KeyCode::Down, 2),
            (KeyCode::Down, 3),
            (KeyCode::Down, 3),
            (KeyCode::Up, 2),
            (KeyCode::Up, 1),
            (KeyCode::Up, 0),
            (KeyCode::Up, 0),
        ] {
            key(&mut app, code);
            wait_for_files(&mut app);
            assert_selected_match(&app, selected, preview);
        }
        assert_eq!(app.tabs.len(), if preview { 1 } else { 3 });
    }
}

#[test]
fn rapid_arrows_reuse_loading_previews_and_keep_the_latest_match_position() {
    let (_root, mut app) = fixture(true);
    app.preview_global_search_result();
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Up] {
        key(&mut app, code);
    }
    wait_for_files(&mut app);
    assert_selected_match(&app, 1, true);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "first needle\n界 needle tail\nlast needle\n"
    );
}

#[test]
fn arrows_skip_collapsed_files_and_handle_missing_or_stale_selections() {
    let (root, mut app) = fixture(true);
    app.focus_global_search_results();
    app.global_search_results
        .set_collapsed(&root.path().join("b.txt"), true);
    for (code, selected) in [
        (KeyCode::Up, 0),
        (KeyCode::Down, 1),
        (KeyCode::Down, 3),
        (KeyCode::Up, 1),
    ] {
        key(&mut app, code);
        wait_for_files(&mut app);
        assert_selected_match(&app, selected, true);
    }
    app.global_search_selected = usize::MAX;
    key(&mut app, KeyCode::Down);
    wait_for_files(&mut app);
    assert_selected_match(&app, 3, true);
    let identity = app.editor().unwrap().navigation_id;
    app.global_search_results.set_all_collapsed(true);
    for code in [KeyCode::Up, KeyCode::Down] {
        key(&mut app, code);
    }
    assert_eq!(app.editor().unwrap().navigation_id, identity);
    assert_eq!(app.tabs.len(), 1);
    app.global_search_results.clear();
    for code in [KeyCode::Up, KeyCode::Down] {
        key(&mut app, code);
    }
    assert_eq!(app.global_search_selected, 0);
    assert_eq!(app.editor().unwrap().navigation_id, identity);
}

#[test]
fn popup_arrows_keep_search_open_and_continue_through_matches() {
    let (_root, mut app) = fixture(true);
    app.show_explorer = false;
    key(&mut app, KeyCode::Tab);
    assert!(app.global_search_results_focused);
    for (code, selected) in [(KeyCode::Down, 1), (KeyCode::Down, 2), (KeyCode::Up, 1)] {
        key(&mut app, code);
        wait_for_files(&mut app);
        assert_eq!(app.global_search_selected, selected);
        assert!(app.global_search_active);
        assert_eq!(app.focused_text_input_id(), None);
        assert_eq!(
            app.editor().unwrap().file_path.as_ref(),
            Some(&app.global_search_results[selected].file_path)
        );
        assert!(app.tabs[app.active_tab].preview);
        assert_eq!(app.tabs.len(), 1);
    }
    key(&mut app, KeyCode::BackTab);
    assert_eq!(app.focused_text_input_id(), Some(TextInputId::GlobalSearch(0)));
    key(&mut app, KeyCode::Tab);
    assert!(app.global_search_results_focused);
    key(&mut app, KeyCode::Enter);
    assert!(!app.global_search_active);
    assert_eq!(app.active_pane, ActivePane::Editor);
}

#[test]
fn enter_focuses_the_editor_and_tab_and_escape_return_to_search_fields() {
    let (_root, mut app) = fixture(true);
    app.focus_text_input(TextInputId::GlobalSearch(3));
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Down);
    wait_for_files(&mut app);
    assert_selected_match(&app, 1, true);
    for code in [KeyCode::Tab, KeyCode::Esc] {
        key(&mut app, code);
        assert_eq!(
            app.focused_text_input_id(),
            Some(TextInputId::GlobalSearch(0))
        );
        key(&mut app, KeyCode::Down);
        assert_eq!(app.focused_text_input_id(), Some(TextInputId::GlobalSearch(0)));
        assert_eq!(app.global_search_selected, 1);
        app.focus_text_input(TextInputId::GlobalSearch(3));
        key(&mut app, KeyCode::Tab);
        assert_selected_match(&app, 1, true);
    }
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert_eq!(app.focused_text_input_id(), None);
    key(&mut app, KeyCode::Up);
    assert_eq!(app.editor().unwrap().cursor_line, 0);
    assert_eq!(app.global_search_selected, 1);
    key(&mut app, KeyCode::Char('!'));
    assert!(app.editor().unwrap().modified);
    assert!(!app.tabs[app.active_tab].preview);
    assert_eq!(app.global_search_query, "needle");
}

#[test]
fn search_fields_browse_history_with_existing_results_without_opening_matches() {
    for sidebar in [false, true] {
        for field in 0..if sidebar { 4 } else { 1 } {
            for modifiers in [KeyModifiers::NONE, KeyModifiers::ALT] {
                let (_root, mut app) = fixture(true);
                app.show_explorer = sidebar;
                app.global_search_replace_visible = true;
                app.focus_text_input(TextInputId::GlobalSearch(field));
                *app.global_search_field_state_mut(field).0 = "draft".into();
                *app.global_search_field_history_mut(field) = crate::search_history::History::restore(
                    &["older".into(), "recent".into()]);
                for (code, expected) in [(KeyCode::Up, "recent"), (KeyCode::Up, "older"),
                    (KeyCode::Down, "recent"), (KeyCode::Down, "draft")] {
                    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code, modifiers));
                    assert_eq!(app.global_search_field_state_mut(field).0, expected);
                    assert_eq!(app.focused_text_input_id(), Some(TextInputId::GlobalSearch(field)));
                    assert!(!app.global_search_results_focused);
                    assert_eq!(app.global_search_selected, 0);
                    assert!(app.tabs.is_empty(), "history navigation must not open a match");
                }
                for modifiers in [KeyModifiers::CONTROL, KeyModifiers::SHIFT] {
                    for code in [KeyCode::Up, KeyCode::Down] {
                        crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code, modifiers));
                        assert_eq!(app.focused_text_input_id(), Some(TextInputId::GlobalSearch(field)));
                        assert!(app.tabs.is_empty());
                    }
                }
            }
        }
    }
}
