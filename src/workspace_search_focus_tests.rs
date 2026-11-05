use super::*;
use crate::search_results::SearchRow;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn click(app: &mut App, column: u16, row: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
}

fn click_result_row(app: &mut App, terminal: &mut Terminal<TestBackend>, hit: SearchRow) {
    draw(app, terminal);
    let row = match hit {
        SearchRow::Header(index) => app.global_search_results.header_row(index).unwrap(),
        SearchRow::Result(index) => app.global_search_results.result_row(index).unwrap(),
    };
    let area = app.explorer_area.unwrap();
    let top = crate::ui_search_panel::search_results_start_y(app, area);
    let y = top + row.saturating_sub(app.global_search_scroll) as u16;
    assert!(y < area.bottom());
    click(app, area.x + 4, y);
}

fn assert_list_focus(app: &App) {
    assert_eq!(app.active_pane, ActivePane::Explorer);
    assert!(!app.global_search_active);
    assert!(!app.search_input_focused);
    assert!(!app.terminal_panel_focused);
    assert_eq!(app.focused_text_input_id(), None);
    assert_eq!(app.global_search_query, "needle");
}

fn assert_open_match(app: &App, index: usize, preview: bool) {
    assert_list_focus(app);
    assert_eq!(
        app.global_search_selected_row(),
        Some(SearchRow::Result(index))
    );
    let result = &app.global_search_results[index];
    let editor = app.editor().unwrap();
    assert_eq!(editor.file_path.as_ref(), Some(&result.file_path));
    assert_eq!(
        (editor.cursor_line, editor.cursor_col),
        (result.line_num - 1, result.col_start)
    );
    assert_eq!(app.tabs[app.active_tab].preview, preview);
}

#[test]
fn clicking_matches_keeps_search_focus_for_arrows_and_honors_preview_tabs() {
    for preview in [false, true] {
        for sidebar_right in [false, true] {
            let (_root, mut app) = fixture(preview);
            app.sidebar_right = sidebar_right;
            app.search_input_focused = true;
            app.terminal_panel_focused = true;
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
            wait_for_files(&mut app);
            assert_open_match(&app, 1, preview);
            for (code, index) in [(KeyCode::Up, 0), (KeyCode::Down, 1), (KeyCode::Down, 2)] {
                key(&mut app, code);
                wait_for_files(&mut app);
                assert_open_match(&app, index, preview);
            }
            assert_eq!(app.tabs.len(), if preview { 1 } else { 2 });
            key(&mut app, KeyCode::Enter);
            assert_eq!(app.active_pane, ActivePane::Editor);
            key(&mut app, KeyCode::Char('!'));
            assert!(app.editor().unwrap().modified);
            assert_eq!(app.global_search_query, "needle");
        }
    }
}

#[test]
fn double_clicking_a_match_keeps_the_tab_open_and_preserves_search_navigation() {
    for preview in [false, true] {
        for sidebar_right in [false, true] {
            for loading in [false, true] {
                let (root, mut app) = fixture(preview);
                app.sidebar_right = sidebar_right;
                if !loading {
                    app.open_file_from_search(&root.path().join("a.txt"));
                    wait_for_files(&mut app);
                    app.focus_global_search();
                }
                let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
                click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
                assert_eq!(app.tabs[app.active_tab].preview, preview);
                assert_eq!(app.editor().unwrap().bg_loading, loading);
                let identity = app.editor().unwrap().navigation_id;
                click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
                assert!(!app.tabs[app.active_tab].preview);
                assert!(!app.tabs[app.active_tab].pinned);
                assert_eq!(app.editor().unwrap().navigation_id, identity);
                assert_eq!(app.tabs.len(), 1);
                wait_for_files(&mut app);
                assert_open_match(&app, 1, false);
                key(&mut app, KeyCode::Down);
                wait_for_files(&mut app);
                assert_open_match(&app, 2, preview);
                assert_eq!(app.tabs.len(), 2);
                let kept = app.tabs.iter().find(|tab| tab.editor.navigation_id == identity).unwrap();
                assert!(!kept.preview);
                assert!(!kept.pinned);
                key(&mut app, KeyCode::Up);
                assert_open_match(&app, 1, false);
            }
        }
    }
}

#[test]
fn popup_double_click_keeps_the_file_open_and_single_click_still_previews() {
    for preview in [false, true] {
        let (_root, mut app) = fixture(preview);
        app.show_explorer = false;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        draw(&mut app, &mut terminal);
        click(&mut app, 19, 5);
        assert_eq!(app.global_search_selected, 1);
        assert_eq!(app.tabs[app.active_tab].preview, preview);
        draw(&mut app, &mut terminal);
        click(&mut app, 19, 5);
        assert!(!app.tabs[app.active_tab].preview);
        assert!(!app.tabs[app.active_tab].pinned);
        wait_for_files(&mut app);
        assert!(app.global_search_active);
        assert_eq!(app.editor().unwrap().cursor_line, 1);
        draw(&mut app, &mut terminal);
        click(&mut app, 19, 6);
        assert_eq!(app.global_search_selected, 2);
        assert_eq!(app.tabs[app.active_tab].preview, preview);
        assert_eq!(app.tabs.len(), 2);
        assert!(!app.tabs[0].preview);
    }
}

#[test]
fn a_different_match_reusing_the_clicked_row_does_not_count_as_a_double_click() {
    let (_root, mut app) = fixture(true);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
    let mut results = app.global_search_results.to_vec();
    results[1].line_num = 3;
    results[1].col_start = 5;
    results[1].col_end = 11;
    app.global_search_results = results.into();
    click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
    assert!(app.tabs[app.active_tab].preview);
    click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
    assert!(!app.tabs[app.active_tab].preview);
    wait_for_files(&mut app);
    assert_open_match(&app, 1, false);
}

#[test]
fn separate_clicks_and_intervening_header_clicks_leave_the_file_in_preview() {
    let (_root, mut app) = fixture(true);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
    std::thread::sleep(Duration::from_millis(450));
    click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
    assert!(app.tabs[app.active_tab].preview);
    click_result_row(&mut app, &mut terminal, SearchRow::Header(0));
    click_result_row(&mut app, &mut terminal, SearchRow::Header(0));
    click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
    assert!(app.tabs[app.active_tab].preview);
    click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
    assert!(!app.tabs[app.active_tab].preview);
    wait_for_files(&mut app);
    assert_open_match(&app, 1, false);
}

#[test]
fn left_and_right_keep_the_same_file_selected_and_down_enters_its_matches() {
    for sidebar_right in [false, true] {
        for replace_visible in [false, true] {
            let (root, mut app) = fixture(true);
            app.sidebar_right = sidebar_right;
            app.global_search_replace_visible = replace_visible;
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
            wait_for_files(&mut app);
            let file = root.path().join("a.txt");
            let identity = app.editor().unwrap().navigation_id;
            for code in [KeyCode::Left, KeyCode::Left] {
                key(&mut app, code);
                assert!(app.is_global_search_file_collapsed(&file));
                assert_eq!(app.global_search_selected_row(), Some(SearchRow::Header(0)));
                assert_list_focus(&app);
                assert_eq!(app.editor().unwrap().navigation_id, identity);
            }
            for code in [KeyCode::Right, KeyCode::Right] {
                key(&mut app, code);
                assert!(!app.is_global_search_file_collapsed(&file));
                assert_eq!(app.global_search_selected_row(), Some(SearchRow::Header(0)));
                assert_list_focus(&app);
            }
            key(&mut app, KeyCode::Down);
            wait_for_files(&mut app);
            assert_open_match(&app, 0, true);
            key(&mut app, KeyCode::Down);
            assert_open_match(&app, 1, true);
            key(&mut app, KeyCode::Left);
            key(&mut app, KeyCode::Down);
            assert_eq!(app.global_search_selected_row(), Some(SearchRow::Header(2)));
            key(&mut app, KeyCode::Left);
            assert!(app.is_global_search_file_collapsed(&root.path().join("b.txt")));
            key(&mut app, KeyCode::Right);
            assert!(!app.is_global_search_file_collapsed(&root.path().join("b.txt")));
            assert!(app.is_global_search_file_collapsed(&file));
            key(&mut app, KeyCode::Down);
            wait_for_files(&mut app);
            assert_open_match(&app, 2, true);
        }
    }
}

#[test]
fn clicked_headers_and_fully_collapsed_lists_remain_navigable() {
    let (root, mut app) = fixture(true);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    app.global_search_results.set_all_collapsed(true);
    click_result_row(&mut app, &mut terminal, SearchRow::Header(2));
    assert_list_focus(&app);
    assert_eq!(app.global_search_selected_row(), Some(SearchRow::Header(2)));
    assert!(!app.is_global_search_file_collapsed(&root.path().join("b.txt")));
    key(&mut app, KeyCode::Left);
    assert!(app.all_global_search_files_collapsed());
    key(&mut app, KeyCode::Down);
    assert_eq!(app.global_search_selected_row(), Some(SearchRow::Header(3)));
    key(&mut app, KeyCode::Up);
    assert_eq!(app.global_search_selected_row(), Some(SearchRow::Header(2)));
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Down);
    wait_for_files(&mut app);
    assert_open_match(&app, 2, true);
}

#[test]
fn header_selection_survives_streamed_reordering_and_dismissal_clears_it() {
    let (root, mut app) = fixture(true);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    click_result_row(&mut app, &mut terminal, SearchRow::Header(2));
    let mut earlier = app.global_search_results[0].clone();
    earlier.file_path = root.path().join("0.txt");
    app.global_search_results.extend([earlier]);
    assert_eq!(app.global_search_selected_row(), Some(SearchRow::Header(3)));
    key(&mut app, KeyCode::Right);
    assert!(!app.is_global_search_file_collapsed(&root.path().join("b.txt")));
    key(&mut app, KeyCode::Delete);
    assert_eq!(
        app.global_search_results
            .file_matches(&root.path().join("b.txt")),
        0
    );
    assert_eq!(
        app.global_search_results
            .file_matches(&root.path().join("a.txt")),
        2
    );
    assert!(app.global_search_selected_header.is_none());
    assert_eq!(
        std::fs::read_to_string(root.path().join("b.txt")).unwrap(),
        "first needle\n界 needle tail\nlast needle\n"
    );
    app.clear_global_search();
    assert!(app.global_search_selected_header.is_none());
}

#[test]
fn left_and_right_edit_search_fields_and_ignore_empty_or_stale_result_selections() {
    let (_root, mut app) = fixture(true);
    app.global_search_replace_query = "replacement".into();
    app.search_include_pattern = "include".into();
    app.search_exclude_pattern = "exclude".into();
    for field in 0..4 {
        app.focus_text_input(TextInputId::GlobalSearch(field));
        app.global_search_field_cursors[usize::from(field)] = 2;
        key(&mut app, KeyCode::Left);
        assert_eq!(app.global_search_field_cursors[usize::from(field)], 1);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.global_search_field_cursors[usize::from(field)], 2);
        assert!(
            !app.global_search_results
                .iter()
                .any(|result| app.is_global_search_file_collapsed(&result.file_path))
        );
    }
    app.focus_global_search_results();
    app.global_search_selected = usize::MAX;
    for code in [KeyCode::Left, KeyCode::Right] {
        key(&mut app, code);
    }
    app.global_search_results.clear();
    for code in [KeyCode::Left, KeyCode::Right] {
        key(&mut app, code);
    }
    assert!(app.tabs.is_empty());
    assert_list_focus(&app);
}

#[test]
fn popup_match_clicks_keep_search_open_for_further_arrow_navigation() {
    let (_root, mut app) = fixture(true);
    app.show_explorer = false;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    draw(&mut app, &mut terminal);
    click(&mut app, 18, 5);
    wait_for_files(&mut app);
    assert_eq!(app.global_search_selected, 1);
    assert!(app.global_search_active);
    assert_eq!(app.focused_text_input_id(), None);
    assert!(app.global_search_results_focused);
    key(&mut app, KeyCode::Down);
    wait_for_files(&mut app);
    assert_eq!(app.global_search_selected, 2);
    assert!(app.global_search_active);
    assert_eq!(
        app.editor().unwrap().file_path.as_ref(),
        Some(&app.global_search_results[2].file_path)
    );
}

#[test]
fn clicking_back_into_the_query_restores_history_navigation() {
    for sidebar in [false, true] {
        let (_root, mut app) = fixture(true);
        app.show_explorer = sidebar;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        if sidebar {
            click_result_row(&mut app, &mut terminal, SearchRow::Result(1));
        } else {
            draw(&mut app, &mut terminal);
            let input = app.text_input_area(TextInputId::GlobalSearch(0)).unwrap();
            click(&mut app, input.x + 2, input.y + 2);
        }
        wait_for_files(&mut app);
        assert_eq!(app.focused_text_input_id(), None);
        assert!(app.global_search_results_focused);
        let identity = app.editor().unwrap().navigation_id;
        draw(&mut app, &mut terminal);
        let input = app.text_input_area(TextInputId::GlobalSearch(0)).unwrap();
        click(&mut app, input.x + 2, input.y);
        assert_eq!(app.focused_text_input_id(), Some(TextInputId::GlobalSearch(0)));
        assert!(!app.global_search_results_focused);
        app.global_search_history = crate::search_history::History::restore(&["older".into(), "recent".into()]);
        key(&mut app, KeyCode::Up);
        assert_eq!(app.global_search_query, "recent");
        assert_eq!(app.editor().unwrap().navigation_id, identity);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.global_search_query, "needle");
        assert_eq!(app.focused_text_input_id(), Some(TextInputId::GlobalSearch(0)));
    }
}

#[test]
fn closing_the_popup_after_result_navigation_restores_editor_focus() {
    for mouse in [false, true] {
        let (_root, mut app) = fixture(true);
        app.show_explorer = false;
        app.preview_global_search_result();
        wait_for_files(&mut app);
        if mouse {
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            draw(&mut app, &mut terminal);
            let input = app.text_input_area(TextInputId::GlobalSearch(0)).unwrap();
            click(&mut app, input.right() - 3, input.y - 1);
        } else {
            key(&mut app, KeyCode::Esc);
        }
        assert!(!app.global_search_active);
        assert!(!app.global_search_results_focused);
        assert_eq!(app.active_pane, ActivePane::Editor);
        assert_eq!(app.focused_text_input_id(), None);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.editor().unwrap().cursor_line, 1);
        assert_eq!(app.global_search_selected, 0);
    }
}
