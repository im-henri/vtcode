use crate::{
    app::{ActivePane, App, GlobalSearchResult, SidebarView, Tab},
    editor::Editor,
    search_results::SearchRow,
    ui_search_controls::{SearchControl, SearchScope},
};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::{path::Path, sync::Arc, time::Duration};

fn search(app: &mut App) {
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert!(app.global_search_error.is_none(), "{:?}", app.global_search_error);
}

fn finish(app: &mut App) {
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.bg_global_replace_running && !app.bg_global_search_running
    });
    assert!(app.global_search_error.is_none(), "{:?}", app.global_search_error);
}

fn draw(app: &mut App, area: Rect) -> Terminal<TestBackend> {
    app.explorer_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    terminal.draw(|frame| crate::ui_search_panel::draw_search_panel(frame, app, area)).unwrap();
    terminal
}

#[test]
fn clicking_replace_changes_only_that_occurrence_and_keeps_other_results_available() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("a.txt");
    std::fs::write(&path, "foo foo\nfoo").unwrap();
    std::fs::write(root.path().join("b.txt"), "foo").unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.source_control_enabled = false;
    app.focus_global_search();
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "bar".into();
    search(&mut app);
    assert_eq!(app.global_search_results.len(), 4);
    let area = Rect::new(5, 3, 40, 24);
    draw(&mut app, area);
    let button = app.search_control_regions.iter().find(|button|
        matches!(button.control, SearchControl::ReplaceResult(1, _))).copied().unwrap();
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column: button.area.x + 1,
        row: button.area.y, modifiers: KeyModifiers::NONE,
    };
    crate::mouse::handle_mouse(&mut app, event);
    assert!(app.bg_global_replace_running);
    assert!(app.tabs.is_empty(), "replacing a result must not open its file");
    assert_eq!(app.active_pane, ActivePane::Explorer);
    let generation = app.global_replace_generation;
    crate::mouse::handle_mouse(&mut app, event);
    assert_eq!(app.global_replace_generation, generation, "an in-flight click cannot replace a neighbor");
    finish(&mut app);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo bar\nfoo");
    assert_eq!(std::fs::read_to_string(root.path().join("b.txt")).unwrap(), "foo");
    assert_eq!(app.global_search_results.len(), 3);
    assert_eq!(app.global_search_selected, 1, "keep the next result selected after refreshing");
    assert_eq!(app.global_search_results[1].line_num, 2);
    draw(&mut app, area);
    let next = app.search_control_regions.iter().find(|button|
        matches!(button.control, SearchControl::ReplaceResult(1, _))).copied().unwrap();
    crate::ui_search_controls::activate(&mut app, SearchScope::Workspace, next.control);
    finish(&mut app);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo bar\nbar");
    assert_eq!(app.global_search_results.len(), 2);
}

#[test]
fn one_match_replacement_supports_closed_files_remote_files_and_undoable_open_drafts() {
    for remote in [false, true] {
        for open in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let native = root.path().join("a.txt");
            std::fs::write(&native, "foo foo\r\n").unwrap();
            std::fs::write(root.path().join("b.txt"), "foo").unwrap();
            let workspace = remote.then(|| crate::remote::workspace::Workspace::new(
                crate::remote::tests::connection(root.path(), Arc::default(), Arc::default())));
            let mut app = App::new_deferred(workspace.as_ref().map_or_else(
                || root.path().into(), |workspace| workspace.view_root.clone()));
            app.remote_workspace = workspace.clone();
            app.source_control_enabled = false;
            let path = workspace.as_ref().map_or_else(|| native.clone(), |workspace| workspace.target_key("a.txt").unwrap());
            if open {
                let mut editor = Editor::test_with_text("foo foo\r\n");
                editor.file_path = Some(path.clone());
                editor.record_saved_snapshot(editor.rope.clone());
                editor.paste_plain_with_text("draft ");
                app.tabs.push(Tab::new_file(editor));
            }
            app.global_search_query = "foo".into();
            app.global_search_replace_query = "猫".into();
            search(&mut app);
            let index = app.global_search_results.iter().enumerate().filter(|(_, found)| found.file_path == path)
                .nth(1).unwrap().0;
            let revision = app.global_search_results.revision();
            app.replace_global_search_result(index, revision);
            finish(&mut app);
            if open {
                assert_eq!(app.editor().unwrap().rope.to_string(), "draft foo 猫\r\n");
                assert_eq!(std::fs::read_to_string(&native).unwrap(), "foo foo\r\n");
                app.editor_mut().unwrap().undo();
                assert_eq!(app.editor().unwrap().rope.to_string(), "draft foo foo\r\n");
            } else {
                assert!(app.tabs.is_empty());
                assert_eq!(std::fs::read_to_string(&native).unwrap(), "foo 猫\r\n");
            }
            assert_eq!(std::fs::read_to_string(root.path().join("b.txt")).unwrap(), "foo");
            if let Some(workspace) = workspace { workspace.client.call(crate::remote::Command::Shutdown).unwrap(); }
        }
    }
}

#[test]
fn single_match_replacement_honors_regex_multiline_unicode_word_case_and_empty_replacements() {
    for (source, query, replacement, index, regex, whole_word, preserve, expected) in [
        ("cat42 cat7\r\nkeep", r"(cat)(\d+)", "$2-$1", 1, true, false, false, "cat42 7-cat\r\nkeep"),
        ("foo\r\nbar foo\r\nbar", r"foo\r?\nbar", "joined", 1, true, false, false, "foo\r\nbar joined"),
        ("FOO foo Foo", "foo", "bar", 0, false, false, true, "BAR foo Foo"),
        ("é界 foo foo", "foo", "🙂", 1, false, false, false, "é界 foo 🙂"),
        ("foo foo", "foo", "", 1, false, false, false, "foo "),
        ("ab\nab", "^", ">", 1, true, false, false, "ab\n>ab"),
        ("foobar foo foo", "foo", "bar", 1, false, true, false, "foobar foo bar"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a.txt");
        std::fs::write(&path, source).unwrap();
        let mut app = App::new_deferred(root.path().into());
        app.source_control_enabled = false;
        app.global_search_query = query.into();
        app.global_search_replace_query = replacement.into();
        app.global_search_regex = regex;
        app.global_search_whole_word = whole_word;
        app.global_search_preserve_case = preserve;
        search(&mut app);
        let revision = app.global_search_results.revision();
        app.replace_global_search_result(index, revision);
        assert!(app.bg_global_replace_running, "{query:?}");
        finish(&mut app);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected, "{query:?} -> {replacement:?}");
    }
}

#[test]
fn stale_hidden_busy_and_refreshing_result_actions_cannot_replace_a_neighbor() {
    for state in 0..6 {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("a.txt");
        std::fs::write(&path, "foo foo").unwrap();
        let mut app = App::new_deferred(root.path().into());
        app.source_control_enabled = false;
        app.global_search_query = "foo".into();
        app.global_search_replace_query = "bar".into();
        search(&mut app);
        let revision = app.global_search_results.revision();
        match state {
            0 => app.dismiss_global_search_result(SearchRow::Result(0)),
            1 => app.global_search_replace_visible = false,
            2 => app.bg_global_search_running = true,
            3 => app.bg_global_replace_running = true,
            4 => app.queue_global_search_model_update(path.clone(), ropey::Rope::from_str("foo")),
            _ => {},
        }
        let generation = app.global_replace_generation;
        app.replace_global_search_result(if state == 5 { usize::MAX } else { 0 }, revision);
        assert_eq!(app.global_replace_generation, generation);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "foo foo");
    }
}

#[test]
fn one_match_replacement_rejects_a_stale_disk_match() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("a.txt");
    std::fs::write(&path, "foo foo").unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.source_control_enabled = false;
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "bar".into();
    search(&mut app);
    std::fs::write(&path, "xxx foo").unwrap();
    let revision = app.global_search_results.revision();
    app.replace_global_search_result(0, revision);
    finish(&mut app);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "xxx foo");
}

#[test]
fn replacing_a_scrolled_result_preserves_the_viewport_and_selects_the_next_match() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("a.txt");
    std::fs::write(&path, "foo\n".repeat(100)).unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.source_control_enabled = false;
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "bar".into();
    search(&mut app);
    app.global_search_scroll = 45;
    draw(&mut app, Rect::new(0, 0, 40, 24));
    let revision = app.global_search_results.revision();
    app.replace_global_search_result(50, revision);
    finish(&mut app);
    assert_eq!(app.global_search_selected, 50);
    assert_eq!(app.global_search_scroll, 45);
    assert_eq!(app.global_search_results[50].line_num, 52);
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().nth(50), Some("bar"));
    assert_eq!(text.matches("foo").count(), 99);
}

#[test]
fn replace_buttons_follow_scrolled_match_rows_and_stay_separate_from_dismiss_and_scrollbars() {
    for right in [false, true] {
        for theme in [crate::app_theme::EditorTheme::Dark, crate::app_theme::EditorTheme::Light] {
            for width in [18, 24, 48] {
                let mut app = App::test_new();
                app.colors = theme.colors();
                app.sidebar_view = SidebarView::Search;
                app.sidebar_right = right;
                app.global_search_query = "foo".into();
                app.global_search_replace_query = "bar".into();
                app.global_search_active = true;
                app.mouse_col = u16::MAX;
                app.mouse_row = u16::MAX;
                app.global_search_results = (0..40).flat_map(|file| (1..=2).map(move |line| GlobalSearchResult {
                    file_path: Path::new("src").join(format!("file{file:02}界.rs")), line_num: line,
                    line_text: "界 foo foo".into(), col_start: 2, col_end: 5,
                    context_before: None, context_after: None,
                })).collect();
                app.global_search_scroll = 17;
                let area = Rect::new(5, 3, width, 24);
                draw(&mut app, area);
                let results_y = crate::ui_search_panel::search_results_start_y(&app, area);
                let buttons: Vec<_> = app.search_control_regions.iter().filter(|button|
                    matches!(button.control, SearchControl::ReplaceResult(_, _))).copied().collect();
                assert!(!buttons.is_empty());
                for button in &buttons {
                    let SearchControl::ReplaceResult(index, revision) = button.control else { unreachable!() };
                    assert_eq!(revision, app.global_search_results.revision());
                    let row = app.global_search_scroll + usize::from(button.area.y - results_y);
                    assert_eq!(app.global_search_results.row_at(row), Some(SearchRow::Result(index)));
                    assert_eq!(button.area.intersection(area), button.area);
                    assert!(button.area.right() < area.right() - u16::from(!right));
                    assert!(!button.disabled);
                }
                app.mouse_col = buttons[0].area.x;
                app.mouse_row = buttons[0].area.y;
                let terminal = draw(&mut app, area);
                let dismiss = app.search_control_regions.iter().find(|button| button.area.y == app.mouse_row
                    && matches!(button.control, SearchControl::DismissResult(_, _))).unwrap();
                assert!(dismiss.area.intersection(buttons[0].area).is_empty());
                assert_eq!(terminal.backend().buffer()[(buttons[0].area.x + 1, app.mouse_row)].symbol(), "↵");
                crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), &format!(
                    "workspace replace {} {} {width}", if right { "right" } else { "left" },
                    if matches!(theme, crate::app_theme::EditorTheme::Light) { "light" } else { "dark" }));
                app.global_search_replace_visible = false;
                draw(&mut app, area);
                assert!(app.search_control_regions.iter().all(|button|
                    !matches!(button.control, SearchControl::ReplaceResult(_, _))));
            }
        }
    }
}
