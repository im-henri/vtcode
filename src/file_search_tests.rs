use crate::{
    app::{App, SplitDirection, Tab},
    editor::Editor,
    text_input::TextInputId,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn app() -> App {
    let mut app = App::test_new();
    app.tabs = vec![
        Tab::new_file(Editor::test_with_text("alpha alpha\n")),
        Tab::new_file(Editor::test_with_text("beta beta\n")),
    ];
    app.active_tab = 0;
    app.active_pane = crate::app::ActivePane::Editor;
    app.show_explorer = false;
    app
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn find(app: &mut App, query: &str) {
    key(app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    key(app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(app.paste_into_focused_text_input(query));
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

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn middle_mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
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

#[test]
fn middle_click_column_selection_returns_keyboard_focus_from_find_and_replace() {
    for wrapped in [false, true] {
        for field in [TextInputId::SearchFind, TextInputId::SearchReplace] {
            let mut app = app();
            app.tabs[0].editor = Editor::test_with_text(&"alpha alpha\n".repeat(3));
            app.editor_mut().unwrap().word_wrap = wrapped;
            find(&mut app, "alpha");
            app.replace_mode = true;
            app.replace_query = "replacement".into();
            app.replace_cursor = app.replace_query.len();
            app.focus_text_input(field);
            let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
            draw(&mut app, &mut terminal);
            assert_eq!(app.focused_text_input_id(), Some(field));
            assert!(app.search_pre_cursor.is_some());
            let inputs = (
                app.search_query.clone(),
                app.search_cursor,
                app.search_selection_anchor,
                app.replace_query.clone(),
                app.replace_cursor,
                app.replace_selection_anchor,
            );
            let area = app.code_area.unwrap();
            let column = area.x + 2;
            middle_mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Middle),
                column,
                area.y,
            );
            assert_eq!(app.active_pane, crate::app::ActivePane::Editor);
            assert!(app.search_active);
            assert!(!app.search_input_focused);
            assert!(app.search_pre_cursor.is_none());
            assert!(app.focused_text_input_id().is_none());
            assert_eq!(
                (
                    app.editor().unwrap().cursor_line,
                    app.editor().unwrap().cursor_col
                ),
                (0, 2)
            );
            middle_mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Middle),
                column,
                area.y + 2,
            );
            middle_mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Middle),
                column,
                area.y + 2,
            );
            assert_eq!(app.editor().unwrap().extra_cursors, [(1, 2), (2, 2)]);
            draw(&mut app, &mut terminal);
            assert_eq!(app.file_search_bars.len(), 1);
            key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().cursor_col, 3);
            assert_eq!(app.editor().unwrap().extra_cursors, [(1, 3), (2, 3)]);
            key(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
            assert_eq!(
                app.editor().unwrap().rope.to_string(),
                "alpXha alpha\n".repeat(3)
            );
            assert_eq!(
                (
                    app.search_query.clone(),
                    app.search_cursor,
                    app.search_selection_anchor,
                    app.replace_query.clone(),
                    app.replace_cursor,
                    app.replace_selection_anchor
                ),
                inputs,
            );
        }
    }
}

#[test]
fn middle_clicking_an_inactive_split_releases_only_that_editors_find_focus() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        let mut app = app();
        app.tabs[0].editor = Editor::test_with_text(&"alpha alpha\n".repeat(3));
        find(&mut app, "alpha");
        let target_group = app.active_group;
        app.create_split_group_dir(1, direction);
        find(&mut app, "beta");
        let other_group = app.active_group;
        let mut terminal = Terminal::new(TestBackend::new(220, 40)).unwrap();
        draw(&mut app, &mut terminal);
        let area = app.groups[target_group].code_area.unwrap();
        let column = area.x + 2;
        middle_mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Middle),
            column,
            area.y,
        );
        assert_eq!(app.active_group, target_group);
        assert!(app.search_active);
        assert!(!app.search_input_focused);
        assert!(app.search_pre_cursor.is_none());
        middle_mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Middle),
            column,
            area.y + 1,
        );
        middle_mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Middle),
            column,
            area.y + 1,
        );
        key(&mut app, KeyCode::Right, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('X'), KeyModifiers::NONE);
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "alpXha alpha\nalpXha alpha\nalpha alpha\n"
        );
        assert_eq!(app.search_query, "alpha");
        let other = &app.groups[other_group].tabs[0];
        assert!(other.file_search.search_input_focused);
        assert!(other.file_search.search_pre_cursor.is_some());
        assert_eq!(other.file_search.search_query, "beta");
        assert_eq!(other.editor.rope.to_string(), "beta beta\n");
    }
}

#[test]
fn tabs_keep_independent_find_replace_matches_and_input_history() {
    let mut app = app();
    find(&mut app, "alpha");
    app.replace_mode = true;
    app.replace_query = "first replacement".into();
    app.search_current = 1;
    app.search_bar_layout.width = 61;
    app.active_tab = 1;
    assert!(!app.search_active);
    assert!(app.search_query.is_empty());
    assert!(app.replace_query.is_empty());
    find(&mut app, "beta");
    app.replace_query = "second replacement".into();
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.search_query, "betax");
    app.active_tab = 0;
    assert_eq!(app.search_query, "alpha");
    assert_eq!(app.replace_query, "first replacement");
    assert_eq!(app.search_matches, [(0, 0, 5), (0, 6, 11)]);
    assert_eq!(app.search_current, 1);
    assert_eq!(app.search_bar_layout.width, 61);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.search_query, "");
    app.active_tab = 1;
    assert_eq!(app.search_query, "betax");
    assert_eq!(app.replace_query, "second replacement");
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.search_query, "beta");
}

#[test]
fn split_find_bars_stay_inside_their_editors_and_clicks_focus_the_owner() {
    for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
        let mut app = app();
        find(&mut app, "alpha");
        let left_owner = app.file_search_owner();
        app.create_split_group_dir(1, direction);
        find(&mut app, "beta");
        let right_group = app.active_group;
        let right_owner = app.file_search_owner();
        app.terminal_sidebar_revealed = true;
        app.show_terminal_sidebar = true;
        app.terminals[0].terminal_pty = Some(crate::terminal::TerminalEmulator::test_with_output(
            40,
            30,
            "shell output\r\n",
        ));
        let mut terminal = Terminal::new(TestBackend::new(220, 40)).unwrap();
        draw(&mut app, &mut terminal);
        assert_eq!(app.file_search_bars.len(), 2);
        let dock = app.terminal_sidebar_area.unwrap();
        for bar in &app.file_search_bars {
            let tab = app
                .tabs
                .iter()
                .chain(app.groups.iter().flat_map(|group| &group.tabs))
                .find(|tab| Some(tab.editor.navigation_id) == bar.owner)
                .unwrap();
            let viewport = tab.file_search.viewport.unwrap();
            assert_eq!(bar.area.intersection(viewport), bar.area);
            assert!(bar.area.intersection(dock).is_empty());
        }
        let left = app
            .file_search_bars
            .iter()
            .find(|bar| bar.owner == left_owner)
            .unwrap()
            .area;
        click(&mut app, left.x + 5, left.y + 1);
        assert_eq!(app.file_search_owner(), left_owner);
        assert_ne!(app.active_group, right_group);
        key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(app.paste_into_focused_text_input("left only"));
        assert_eq!(app.search_query, "left only");
        app.switch_group(right_group);
        assert_eq!(app.file_search_owner(), right_owner);
        assert_eq!(app.search_query, "beta");
        assert_eq!(app.search_matches, [(0, 0, 4), (0, 5, 9)]);
        assert!(app.text_input_area(TextInputId::SearchFind).is_some());
    }
}

#[test]
fn closing_an_inactive_panes_find_bar_only_closes_that_bar() {
    let mut app = app();
    find(&mut app, "alpha");
    let owner = app.file_search_owner();
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    find(&mut app, "beta");
    let other_group = app.active_group;
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    draw(&mut app, &mut terminal);
    let close = app
        .search_control_regions
        .iter()
        .find(|button| {
            button.owner == owner
                && button.control == crate::ui_search_controls::SearchControl::Close
        })
        .unwrap()
        .area;
    click(&mut app, close.x + 1, close.y);
    assert_eq!(app.file_search_owner(), owner);
    assert!(!app.search_active);
    draw(&mut app, &mut terminal);
    assert_eq!(app.file_search_bars.len(), 1);
    app.switch_group(other_group);
    assert!(app.search_active);
    assert_eq!(app.search_query, "beta");
}

#[test]
fn background_search_results_return_to_their_tab_after_focus_changes() {
    let mut app = app();
    find(&mut app, "alpha");
    let owner = app.file_search_owner().unwrap();
    let target = app.editor_content_target().unwrap();
    app.file_search_generation = 7;
    app.bg_file_search_running = true;
    app.active_tab = 1;
    find(&mut app, "beta");
    assert!(app.apply_tab_search_matches(owner, 7, target.clone(), vec![(0, 6, 11)], false, None));
    assert_eq!(app.search_query, "beta");
    assert_eq!(app.search_matches, [(0, 0, 4), (0, 5, 9)]);
    assert_eq!(app.tabs[0].file_search.search_matches, [(0, 6, 11)]);
    assert!(!app.tabs[0].file_search.bg_file_search_running);
    app.force_close_tab(0);
    assert!(!app.apply_tab_search_matches(owner, 7, target, vec![], false, None));
    assert_eq!(app.search_query, "beta");
}

#[test]
fn opening_settings_preserves_the_files_find_widget() {
    let mut app = app();
    find(&mut app, "alpha");
    app.open_settings_tab();
    assert!(!app.search_active);
    app.active_tab = 0;
    assert!(app.search_active);
    assert_eq!(app.search_query, "alpha");
}

#[test]
fn a_terminal_pane_keeps_the_other_panes_find_bar_and_owns_typing() {
    let mut app = app();
    find(&mut app, "alpha");
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    app.tabs[0].kind = crate::app::TabKind::Terminal;
    app.tabs[0].terminal_idx = Some(0);
    let mut pty =
        crate::terminal::TerminalEmulator::test_with_output(80, 30, "terminal output\r\n");
    let input = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    draw(&mut app, &mut terminal);
    assert_eq!(app.file_search_bars.len(), 1);
    assert!(
        app.file_search_bars[0]
            .area
            .intersection(app.viewport.unwrap())
            .is_empty()
    );
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(&*input.lock().unwrap(), b"x");
    assert_eq!(app.groups[0].tabs[0].file_search.search_query, "alpha");
}

#[test]
fn an_inactive_panes_changed_content_refreshes_its_matches_without_moving_its_cursor() {
    let mut app = app();
    find(&mut app, "alpha");
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    find(&mut app, "beta");
    let editor = &mut app.groups[0].tabs[0].editor;
    editor.set_cursor(0, 0);
    editor.selection_anchor = None;
    editor.paste_plain_with_text("alpha ");
    let cursor = (editor.cursor_line, editor.cursor_col);
    // A deferred edit scan must finish even after focus leaves this pane.
    app.groups[0].tabs[0]
        .file_search
        .file_search_refresh_pending = true;
    app.groups[0].tabs[0].file_search.file_search_key = None;
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).unwrap();
    draw(&mut app, &mut terminal);
    assert_eq!(app.groups[0].tabs[0].file_search.search_matches.len(), 3);
    let editor = &app.groups[0].tabs[0].editor;
    assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
    assert_eq!(app.search_query, "beta");
}
