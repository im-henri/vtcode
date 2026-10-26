use super::*;
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

#[test]
fn filename_highlights_keep_exact_unicode_columns_with_sparse_and_duplicate_matches() {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let style = Style::default().fg(Color::Blue);
    let highlight = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    for name in ["", "ascii_name.rs", "组件_café_e\u{301}_👩‍👩‍👧‍👦.py", "a very long filename.txt"] {
        for indices in [vec![], vec![0], vec![0, 0, 1, 4, 4, 9], vec![1, 3, 5, 10, 300], vec![999]] {
            let spans = highlighted_name(name, &indices, style, highlight);
            assert_eq!(spans.iter().map(|s| s.content.as_ref()).collect::<String>(), name);
            assert_eq!(spans.iter().map(|span| UnicodeWidthStr::width(span.content.as_ref())).sum::<usize>(),
                UnicodeWidthStr::width(name), "highlighting must preserve scroll width");
            let mut column = 0;
            for span in spans {
                assert!(matches!(span.content, std::borrow::Cow::Borrowed(_)));
                for grapheme in span.content.graphemes(true) {
                    let end = column + grapheme.chars().count();
                    let matched = (column..end).any(|index| indices.contains(&index));
                    assert_eq!(span.style, if matched { highlight } else { style });
                    column = end;
                }
            }
        }
    }
}

fn fixture() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "src/deep/app_input.rs",
        "src/deep/unrelated.txt",
        "src/app.rs",
        "docs/readme.md",
        ".hidden/app_config.rs",
        "zeta.txt",
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "source\n").unwrap();
    }
    let mut app = App::new(dir.path().to_path_buf());
    app.explorer_width = 36;
    app.show_panel = false;
    app.active_pane = ActivePane::Explorer;
    (dir, app)
}

fn wait(app: &mut App) {
    app.wait_for_background(std::time::Duration::from_secs(10), |app| {
        !app.bg_explorer_refresh_running
    });
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
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

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, app))
        .unwrap()
        .buffer
        .clone()
}

fn query(app: &mut App, text: &str) {
    app.explorer.find.query = text.into();
    app.explorer.find.cursor = text.len();
    app.explorer.find.anchor = None;
    app.update_explorer_find();
    wait(app);
}

fn capture(buffer: &Buffer, name: &str) {
    let Some(folder) = std::env::var_os("VTCODE_EXPLORER_FIND_CAPTURE") else {
        return;
    };
    let cells: Vec<_> = buffer
        .content
        .iter()
        .map(|cell| {
            serde_json::json!({
                "text":cell.symbol(), "fg":format!("{:?}", cell.fg), "bg":format!("{:?}", cell.bg),
                "bold":cell.modifier.contains(Modifier::BOLD)
            })
        })
        .collect();
    let folder = std::path::PathBuf::from(folder);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join(format!("explorer-{name}.json")),
        serde_json::to_vec(&serde_json::json!({
            "width":buffer.area.width,"height":buffer.area.height,"cells":cells
        }))
        .unwrap(),
    )
    .unwrap();
}

fn rect_text(buffer: &Buffer, area: Rect) -> String {
    (area.y..area.bottom()).flat_map(|y| {
        (area.x..area.right()).map(move |x| buffer[(x, y)].symbol())
    }).collect()
}

#[test]
fn find_mode_remains_obvious_without_focus_and_show_all_restores_the_tree() {
    for width in [20, 36] {
        for right in [false, true] {
            for light in [false, true] {
                let (_dir, mut app) = fixture();
                if light {
                    app.colors = crate::app_theme::EditorTheme::Light.colors();
                }
                app.explorer_width = width;
                app.sidebar_right = right;
                let normal_paths: Vec<_> = app.explorer.entries.iter().map(|e| e.path.clone()).collect();
                app.open_explorer_find();
                app.explorer.find.filter = true;
                query(&mut app, "app");
                app.active_pane = ActivePane::Editor;
                app.explorer.find.focus = Focus::Tree;
                mouse(&mut app, MouseEventKind::Moved, 50, 2);
                let buffer = draw(&mut app, 100, 28);
                let controls = layout(&app, app.explorer_area.unwrap());
                assert!(rect_text(&buffer, controls.title).contains("Find by Name"));
                assert!(rect_text(&buffer, controls.status).contains("Files hidden"));
                let border = &buffer[(controls.panel.x, controls.panel.y)];
                assert_eq!(border.symbol(), "╭");
                assert_ne!(border.fg, border.bg);
                if width == 36 {
                    assert!(rect_text(&buffer, controls.close).contains("Show All"));
                    assert!(rect_text(&buffer, controls.status).contains("3 matches"));
                }
                capture(&buffer, &format!("mode-width-{width}-right-{right}-light-{light}"));
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left),
                    controls.close.x + 1, controls.close.y);
                assert!(!app.explorer.find.open);
                assert!(app.explorer.find.query.is_empty());
                assert!(app.explorer.find.saved_view.is_none());
                assert_eq!(app.explorer.entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
                    normal_paths, "restores files immediately, before the asynchronous refresh");
                let buffer = draw(&mut app, 100, 28);
                assert!(!buffer.content.iter().map(|cell| cell.symbol()).collect::<String>()
                    .contains("Files hidden"));
                wait(&mut app);
            }
        }
    }
}

#[test]
fn find_status_distinguishes_empty_highlight_filtered_loading_and_error_states() {
    let (_dir, mut app) = fixture();
    app.open_explorer_find();
    app.explorer.find.filter = true;
    let buffer = draw(&mut app, 100, 28);
    let controls = layout(&app, app.explorer_area.unwrap());
    assert!(rect_text(&buffer, controls.status).contains("Filter ready"));
    assert!(!rect_text(&buffer, controls.status).contains("Files hidden"));
    capture(&buffer, "mode-empty");
    query(&mut app, "app");
    app.explorer.find.loading = true;
    let buffer = draw(&mut app, 100, 28);
    assert!(rect_text(&buffer, controls.status).contains("Files hidden · searching"));
    assert!(!rect_text(&buffer, controls.status).contains("3 matches"));
    app.explorer.find.loading = false;
    query(&mut app, "");
    let buffer = draw(&mut app, 100, 28);
    assert!(rect_text(&buffer, controls.status).contains("Filter ready"));
    assert!(app.explorer.entries.iter().any(|entry| entry.name == "zeta.txt"));
    app.explorer.find.filter = false;
    query(&mut app, "app");
    let buffer = draw(&mut app, 100, 28);
    assert!(rect_text(&buffer, controls.status).contains("3 matches · highlight only"));
    assert!(!rect_text(&buffer, controls.close).contains("Show All"));
    capture(&buffer, "mode-highlight");
    app.explorer.find.filter = true;
    app.explorer.find.regex = true;
    app.explorer.find.fuzzy = false;
    query(&mut app, "[");
    let buffer = draw(&mut app, 100, 28);
    let controls = layout(&app, app.explorer_area.unwrap());
    assert!(rect_text(&buffer, controls.status).contains("Files hidden · invalid pattern"));
    assert!(rect_text(&buffer, controls.error).contains("Invalid regex:"));
    capture(&buffer, "mode-error");
}

#[test]
fn shortcut_typing_navigation_and_escape_stay_in_explorer() {
    let (_dir, mut app) = fixture();
    key(
        &mut app,
        KeyCode::Char('f'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert!(app.explorer.find.open);
    assert_eq!(app.focused_text_input_id(), Some(TextInputId::ExplorerFind));
    for ch in "app".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    wait(&mut app);
    assert_eq!(app.explorer.find.query, "app");
    assert_eq!(app.explorer.find.results.count, 3);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
    wait(&mut app);
    assert!(app.explorer.find.filter);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.focused_text_input_id(), None);
    assert_eq!(
        app.explorer.entries[app.explorer.selected].name,
        "app_config.rs"
    );
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.explorer.entries[app.explorer.selected].name,
        "app_input.rs"
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.tabs.iter().any(|tab| {
            tab.editor
                .file_path
                .as_ref()
                .is_some_and(|path| path.ends_with("app_input.rs"))
        })
    });
    app.active_pane = ActivePane::Explorer;
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    wait(&mut app);
    assert!(!app.explorer.find.open);
    assert!(app.explorer.entries.iter().all(|entry| entry.depth == 0));
    app.active_pane = ActivePane::Editor;
    assert!(!handle_key(
        &mut app,
        KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL | KeyModifiers::ALT
        )
    ));
}

#[test]
fn context_menu_on_rows_headers_and_blank_space_opens_find() {
    for location in ["file", "folder", "root", "blank", "header"] {
        let (_dir, mut app) = fixture();
        draw(&mut app, 100, 28);
        let area = app.explorer_area.unwrap();
        let content = crate::ui_chrome::explorer_tree_content_area(&app, area);
        let (_, tree) = crate::ui_chrome::explorer_sections(&app, area);
        let row = match location {
            "file" => {
                content.y + app.explorer.entries.iter().position(|e| !e.is_dir).unwrap() as u16
            }
            "folder" => content.y,
            "root" => tree.y,
            "header" => area.y,
            _ => content.bottom() - 1,
        };
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            content.x + 3,
            row,
        );
        assert!(app.explorer_context_active, "{location}");
        let action = app
            .explorer_context_menu_items()
            .iter()
            .position(|&label| label == "Find by Name…")
            .unwrap();
        app.explorer_context_selected = action;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.explorer.find.open, "{location}");
        assert_eq!(app.focused_text_input_id(), Some(TextInputId::ExplorerFind));
    }
}

#[test]
fn find_in_folder_click_opens_search_with_the_folder_included() {
    for workspace_root in [false, true] {
        let (dir, mut app) = fixture();
        app.open_explorer_find();
        wait(&mut app);
        draw(&mut app, 100, 28);
        let area = app.explorer_area.unwrap();
        let content = crate::ui_chrome::explorer_tree_content_area(&app, area);
        let (_, tree) = crate::ui_chrome::explorer_sections(&app, area);
        let row = if workspace_root {
            tree.y
        } else {
            content.y
                + app
                    .explorer
                    .entries
                    .iter()
                    .position(|e| e.name == "src")
                    .unwrap() as u16
        };
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            content.x + 3,
            row,
        );
        let buffer = draw(&mut app, 100, 28);
        capture(&buffer, "find-in-folder-menu");
        let label = "Find in Folder…";
        let label_width = label.chars().count() as u16;
        let (column, row) = (0..28)
            .find_map(|row| {
                (0..100 - label_width).find_map(|column| {
                    let text: String = (column..column + label_width)
                        .map(|x| buffer[(x, row)].symbol())
                        .collect();
                    (text == label).then_some((column, row))
                })
            })
            .expect("Find in Folder is visible");
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            column,
            row,
        );

        assert!(!app.explorer_context_active);
        assert_eq!(app.sidebar_view, SidebarView::Search);
        assert_eq!(app.activity_bar_selected, 1);
        assert_eq!(
            app.focused_text_input_id(),
            Some(TextInputId::GlobalSearch(0))
        );
        assert_eq!(
            app.search_include_pattern,
            if workspace_root { "./" } else { "./src" }
        );
        let buffer = draw(&mut app, 100, 28);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("files to include"));
        assert!(text.contains(&app.search_include_pattern));
        capture(&buffer, "find-in-folder-search");

        // Typing immediately enters the Search query, including when the old
        // Explorer find widget is still open in the other sidebar view.
        for ch in "source".chars() {
            key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(app.global_search_query, "source");
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_global_search_running
        });
        assert_eq!(
            app.global_search_results.len(),
            if workspace_root { 6 } else { 3 }
        );
        assert!(app.global_search_results.iter().all(|result| {
            result.file_path.starts_with(if workspace_root {
                dir.path().to_path_buf()
            } else {
                dir.path().join("src")
            })
        }));
    }
}

#[test]
fn buttons_filter_fuzzy_no_matches_and_close_render_and_work() {
    let (_dir, mut app) = fixture();
    app.open_explorer_find();
    query(&mut app, "air");
    assert_eq!(app.explorer.find.results.count, 2);
    capture(&draw(&mut app, 100, 28), "highlight");
    let areas = layout(&app, app.explorer_area.unwrap());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        areas.filter.x + 2,
        areas.filter.y,
    );
    wait(&mut app);
    assert!(app.explorer.find.filter);
    mouse(&mut app, MouseEventKind::Moved, 70, 2);
    let buffer = draw(&mut app, 100, 28);
    capture(&buffer, "filter");
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::UNDERLINED))
    );
    let content = crate::ui_chrome::explorer_tree_content_area(&app, app.explorer_area.unwrap());
    let index = app
        .explorer
        .entries
        .iter()
        .position(|e| e.name == "app_input.rs")
        .unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Right),
        content.x + 5,
        content.y + index as u16,
    );
    assert_eq!(
        app.explorer_context_target
            .as_ref()
            .unwrap()
            .file_name()
            .unwrap(),
        "app_input.rs"
    );
    app.explorer_context_active = false;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        areas.fuzzy.x + 2,
        areas.fuzzy.y,
    );
    wait(&mut app);
    assert!(!app.explorer.find.fuzzy);
    assert!(app.explorer.entries.is_empty());
    mouse(&mut app, MouseEventKind::Moved, 70, 2);
    let buffer = draw(&mut app, 100, 28);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("No matches"));
    capture(&buffer, "no-matches");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        areas.close.x + 1,
        areas.close.y,
    );
    assert!(!app.explorer.find.open);
    wait(&mut app);
    assert!(!app.explorer.entries.is_empty());
}

#[test]
fn regex_buttons_shortcuts_and_focus_navigation_are_distinct_from_fuzzy_matching() {
    let (_dir, mut app) = fixture();
    app.open_explorer_find();
    let buffer = draw(&mut app, 100, 28);
    assert!(
        buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .contains("Find by Name")
    );
    let controls = layout(&app, app.explorer_area.unwrap());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        controls.regex.x + 2,
        controls.regex.y,
    );
    assert!(app.explorer.find.regex && !app.explorer.find.fuzzy);
    query(&mut app, r"^app.*\.rs$");
    assert_eq!(app.explorer.find.results.count, 3);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
    wait(&mut app);
    let buffer = draw(&mut app, 100, 28);
    capture(&buffer, "regex-filter");
    assert!(app.explorer.find.filter);
    assert_eq!(
        app.explorer
            .entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .count(),
        3
    );
    assert!(
        app.explorer
            .entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .all(|entry| entry.name.starts_with("app"))
    );

    for _ in 0..3 {
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    }
    assert!(matches!(app.explorer.find.focus, Focus::Regex));
    key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    wait(&mut app);
    assert!(!app.explorer.find.regex && !app.explorer.find.fuzzy);
    assert_eq!(
        app.explorer.find.results.count, 0,
        "regex punctuation is literal when both modes are off"
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    wait(&mut app);
    assert!(app.explorer.find.regex);
    assert_eq!(app.explorer.find.results.count, 3);
    key(&mut app, KeyCode::Char('m'), KeyModifiers::ALT);
    assert!(app.explorer.find.fuzzy && !app.explorer.find.regex);
    query(&mut app, "air");
    assert_eq!(app.explorer.find.results.count, 2);
    key(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
    wait(&mut app);
    assert!(app.explorer.find.regex && !app.explorer.find.fuzzy);
    assert_eq!(app.explorer.find.results.count, 0);
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    assert!(!app.global_search_active);
}

#[test]
fn regex_errors_are_visible_and_clear_on_edit_mode_change_and_close() {
    let (_dir, mut app) = fixture();
    app.open_explorer_find();
    key(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
    query(&mut app, "[");
    assert!(app.explorer.find.results.error.is_some());
    let buffer = draw(&mut app, 100, 28);
    capture(&buffer, "regex-error");
    let text = buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Invalid regex:"));
    assert!(!text.contains("No matches"));
    let controls = layout(&app, app.explorer_area.unwrap());
    assert_eq!(controls.error.height, 1);
    assert_eq!(
        crate::ui_chrome::explorer_tree_content_area(&app, app.explorer_area.unwrap()).y,
        controls.panel.bottom()
    );

    query(&mut app, r"\.rs$");
    assert!(app.explorer.find.results.error.is_none());
    assert_eq!(app.explorer.find.results.count, 3);
    draw(&mut app, 100, 28);
    assert_eq!(layout(&app, app.explorer_area.unwrap()).error.height, 0);
    query(&mut app, "[");
    key(&mut app, KeyCode::Char('m'), KeyModifiers::ALT);
    wait(&mut app);
    assert!(app.explorer.find.results.error.is_none());
    assert!(app.explorer.find.fuzzy && !app.explorer.find.regex);
    key(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    wait(&mut app);
    assert!(app.explorer.find.results.error.is_none());
    assert!(!app.explorer.find.open);
    assert!(!app.explorer.entries.is_empty());
}

#[test]
fn superseded_regex_errors_do_not_replace_newer_results() {
    let (_dir, mut app) = fixture();
    app.open_explorer_find();
    key(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
    for text in [r"\.rs$", "[", r"^zeta\.txt$"] {
        app.explorer.find.query = text.into();
        app.update_explorer_find();
    }
    wait(&mut app);
    assert!(app.explorer.find.results.error.is_none());
    assert_eq!(app.explorer.find.results.count, 1);
    assert_eq!(app.explorer.entries.len(), 1);
    assert_eq!(app.explorer.entries[0].name, "zeta.txt");
}

#[test]
fn regex_control_remains_clickable_in_narrow_explorers_on_either_side() {
    for width in [20, 36] {
        for right in [false, true] {
            let (_dir, mut app) = fixture();
            app.explorer_width = width;
            app.sidebar_right = right;
            app.open_explorer_find();
            draw(&mut app, 100, 28);
            let controls = layout(&app, app.explorer_area.unwrap());
            assert!(controls.regex.width >= 3);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                controls.regex.x + 1,
                controls.regex.y,
            );
            query(&mut app, r"\.rs$");
            assert!(app.explorer.find.regex && !app.explorer.find.fuzzy);
            assert_eq!(app.explorer.find.results.count, 3);
            capture(
                &draw(&mut app, 100, 28),
                &format!("regex-width-{width}-right-{right}"),
            );
        }
    }
}

#[test]
fn filtered_folder_toggles_reuse_results_without_scheduling_scans() {
    let (dir, mut app) = fixture();
    app.open_explorer_find();
    app.explorer.find.filter = true;
    query(&mut app, "app");
    draw(&mut app, 100, 28);
    let generation = app.explorer_refresh_generation;
    let index_generation = app.workspace_file_index_generation;
    let content = crate::ui_chrome::explorer_tree_content_area(&app, app.explorer_area.unwrap());
    let src = app
        .explorer
        .entries
        .iter()
        .position(|entry| entry.name == "src")
        .unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        content.x + 2,
        content.y + src as u16,
    );
    assert!(
        !app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "app.rs")
    );
    assert!(!app.bg_explorer_refresh_running);
    assert_eq!(app.explorer_refresh_generation, generation);

    // Keyboard expansion must be immediate too, without waiting for a worker.
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert!(
        app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "app_input.rs")
    );
    let deep = app
        .explorer
        .entries
        .iter()
        .position(|entry| entry.name == "deep")
        .unwrap();
    app.set_explorer_single_selection(deep);
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert!(
        !app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "app_input.rs")
    );
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    assert_eq!(app.explorer.entries[app.explorer.selected].name, "src");
    key(&mut app, KeyCode::Left, KeyModifiers::NONE);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    assert!(
        app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "app.rs")
    );
    assert!(
        !app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "app_input.rs")
    );

    // Reopening cached descendants must use current Git decorations.
    app.explorer
        .apply_git_status_changes(Some(dir.path()), [("src/deep/app_input.rs", 'M')]);
    let deep = app
        .explorer
        .entries
        .iter()
        .position(|entry| entry.name == "deep")
        .unwrap();
    app.set_explorer_single_selection(deep);
    key(&mut app, KeyCode::Right, KeyModifiers::NONE);
    let file = app
        .explorer
        .entries
        .iter()
        .find(|entry| entry.name == "app_input.rs")
        .unwrap();
    assert_eq!(file.git_status, crate::explorer::GitStatus::Modified);
    assert_eq!(app.explorer.find.results.count, 3);
    assert_eq!(app.explorer_refresh_generation, generation);
    assert_eq!(app.workspace_file_index_generation, index_generation);
    assert!(!app.bg_explorer_refresh_running);
    assert!(!app.explorer.find.loading);
}

#[test]
fn collapse_results_button_is_local_and_accessible_on_both_sides() {
    for width in [20, 36] {
        for right in [false, true] {
            let (dir, mut app) = fixture();
            app.explorer_width = width;
            app.sidebar_right = right;
            app.open_explorer_find();
            app.explorer.find.filter = true;
            query(&mut app, "app");
            let selected = app
                .explorer
                .entries
                .iter()
                .position(|entry| entry.name == "app_input.rs")
                .unwrap();
            app.set_explorer_single_selection(selected);
            let buffer = draw(&mut app, 100, 28);
            let controls = layout(&app, app.explorer_area.unwrap());
            assert_eq!(
                buffer
                    .cell((controls.collapse.x + 1, controls.collapse.y))
                    .unwrap()
                    .symbol(),
                "⊟"
            );
            assert!(controls.regex.right() <= controls.collapse.x);
            let generation = app.explorer_refresh_generation;
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                controls.collapse.x + 1,
                controls.collapse.y,
            );
            assert_eq!(
                app.explorer
                    .entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>(),
                [".hidden", "src"]
            );
            assert_eq!(app.explorer.entries[app.explorer.selected].name, "src");
            assert_eq!(app.explorer_multi_selected, [dir.path().join("src")]);
            assert_eq!(app.explorer.find.results.count, 3);
            assert!(
                app.explorer
                    .find
                    .collapsed
                    .contains(&dir.path().join("src/deep"))
            );
            assert_eq!(app.focused_text_input_id(), Some(TextInputId::ExplorerFind));
            let hovered = draw(&mut app, 100, 28);
            assert!(
                hovered
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("Collapse All Results")
            );
            mouse(&mut app, MouseEventKind::Moved, 50, 2);
            capture(
                &draw(&mut app, 100, 28),
                &format!("collapsed-width-{width}-right-{right}"),
            );

            // Collapse All also folds descendants hidden by an already folded parent.
            app.toggle_or_open_explorer_async();
            assert!(
                app.explorer
                    .entries
                    .iter()
                    .any(|entry| entry.name == "deep" && !entry.expanded)
            );
            assert!(
                !app.explorer
                    .entries
                    .iter()
                    .any(|entry| entry.name == "app_input.rs")
            );
            for _ in 0..4 {
                key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            }
            assert!(app.explorer.find.focus == Focus::Collapse);
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.explorer
                    .entries
                    .iter()
                    .all(|entry| !entry.expanded && entry.depth == 0)
            );
            assert_eq!(app.explorer_refresh_generation, generation);
            assert!(!app.bg_explorer_refresh_running);

            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            wait(&mut app);
            assert!(
                app.explorer
                    .entries
                    .iter()
                    .all(|entry| !entry.expanded && entry.depth == 0)
            );
            assert!(
                app.explorer
                    .entries
                    .iter()
                    .any(|entry| entry.name == "docs")
            );
            assert!(app.explorer.find.results.filtered_tree.is_none());
        }
    }
}

#[test]
fn filesystem_refresh_preserves_newer_local_folds_and_updates_cached_results() {
    let (dir, mut app) = fixture();
    app.open_explorer_find();
    app.explorer.find.filter = true;
    app.explorer.find.regex = true;
    app.explorer.find.fuzzy = false;
    query(&mut app, r"^app.*\.rs$");
    std::fs::write(dir.path().join("src/deep/app_new.rs"), "new").unwrap();
    app.refresh_explorer_async();
    let generation = app.explorer_refresh_generation;
    app.collapse_all_explorer_async();
    // The worker captured the expanded tree; its completion must not reopen it.
    assert!(
        app.explorer
            .entries
            .iter()
            .all(|entry| !entry.expanded && entry.depth == 0)
    );
    assert_eq!(app.explorer_refresh_generation, generation);
    wait(&mut app);
    assert_eq!(app.explorer.find.results.count, 4);
    assert!(
        app.explorer
            .entries
            .iter()
            .all(|entry| !entry.expanded && entry.depth == 0)
    );
    for name in ["src", "deep"] {
        let index = app
            .explorer
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .unwrap();
        app.set_explorer_single_selection(index);
        app.toggle_or_open_explorer_async();
    }
    assert!(
        app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "app_new.rs")
    );
    assert_eq!(app.explorer_refresh_generation, generation);
    assert!(!app.bg_explorer_refresh_running);

    query(&mut app, "readme");
    assert_eq!(app.explorer.find.results.count, 1);
    assert!(
        app.explorer
            .entries
            .iter()
            .any(|entry| entry.name == "readme.md")
    );
    assert!(
        !app.explorer
            .entries
            .iter()
            .any(|entry| entry.name.starts_with("app"))
    );
    app.collapse_all_explorer_async();
    app.explorer.selected = 0;
    app.toggle_or_open_explorer_async();
    assert_eq!(app.explorer.entries.last().unwrap().name, "readme.md");
}

#[test]
fn collapse_results_is_inactive_without_filtered_results() {
    let (_dir, mut app) = fixture();
    app.open_explorer_find();
    for (filter, query_text) in [(false, "app"), (true, ""), (true, "no-matches")] {
        app.explorer.find.filter = filter;
        query(&mut app, query_text);
        let generation = app.explorer_refresh_generation;
        let paths: Vec<_> = app
            .explorer
            .entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        assert!(!can_collapse_results(&app));
        activate(&mut app, Focus::Collapse);
        assert_eq!(app.explorer_refresh_generation, generation);
        assert_eq!(
            app.explorer
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>(),
            paths
        );
    }
}

#[test]
fn superseded_searches_and_filesystem_refreshes_use_the_current_query() {
    let (dir, mut app) = fixture();
    app.open_explorer_find();
    app.explorer.find.filter = true;
    for text in ["app", "readme", "zeta"] {
        app.explorer.find.query = text.into();
        app.update_explorer_find();
    }
    wait(&mut app);
    assert_eq!(
        app.explorer
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["zeta.txt"]
    );
    std::fs::write(dir.path().join("zeta2.txt"), "new").unwrap();
    app.refresh_explorer_async();
    wait(&mut app);
    assert_eq!(app.explorer.find.results.count, 2);
    app.explorer.find.query = "app".into();
    app.update_explorer_find();
    app.close_explorer_find();
    wait(&mut app);
    assert!(!app.explorer.find.open);
    assert!(app.explorer.entries.iter().any(|e| e.name == "docs"));
}

#[test]
fn find_geometry_keeps_scrollbar_drags_off_the_resize_seam_on_both_sides() {
    for right in [false, true] {
        let (dir, mut app) = fixture();
        for i in 0..80 {
            std::fs::write(dir.path().join(format!("match{i:02}.rs")), "file").unwrap();
        }
        app.sidebar_right = right;
        app.open_explorer_find();
        app.explorer.find.filter = true;
        query(&mut app, "match");
        draw(&mut app, 100, 28);
        let area = app.explorer_area.unwrap();
        let content = crate::ui_chrome::explorer_tree_content_area(&app, area);
        let bar = crate::ui_chrome::explorer_scrollbar(
            &app,
            area,
            crate::ui_chrome::ExplorerSection::Files,
        )
        .unwrap();
        assert_eq!(bar.area.y, content.y);
        assert_eq!(bar.area.height, content.height);
        assert_eq!(app.explorer.visible_height, content.height as usize);
        let seam = if right { area.x } else { area.right() - 1 };
        assert_ne!(bar.area.x, seam);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            bar.area.x,
            bar.area.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            seam,
            content.bottom(),
        );
        assert_eq!(app.explorer.scroll_offset, 80 - content.height as usize);
        assert!(!app.resizing_explorer);
        assert_eq!(app.explorer_width, 36);
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            seam,
            content.bottom(),
        );
        assert!(app.explorer_scrollbar_drag.is_none());
        draw(&mut app, 100, 28);
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Right),
            content.x + 4,
            content.bottom() - 1,
        );
        assert!(
            app.explorer_context_target
                .as_ref()
                .unwrap()
                .ends_with("match79.rs")
        );
    }
}

#[test]
fn controls_remain_inside_short_and_narrow_explorers() {
    let (_dir, mut app) = fixture();
    app.open_explorer_find();
    for width in [1, 8, 18, 28, 36] {
        for height in [1, 2, 3, 4, 5, 8, 20] {
            let area = Rect::new(2, 2, width, height);
            let controls = layout(&app, area);
            for rect in [
                controls.panel,
                controls.title,
                controls.input,
                controls.filter,
                controls.fuzzy,
                controls.regex,
                controls.collapse,
                controls.close,
                controls.status,
                controls.error,
            ] {
                if !rect.is_empty() {
                    assert_eq!(rect.intersection(area), rect);
                }
            }
            let mut terminal = Terminal::new(TestBackend::new(width + 4, height + 4)).unwrap();
            terminal
                .draw(|frame| crate::ui_chrome::draw_explorer(frame, &mut app, area))
                .unwrap();
        }
    }
}

#[test]
fn creating_items_from_filtered_tree_keeps_inline_naming_visible() {
    for action in ["New File", "New Folder"] {
        let (dir, mut app) = fixture();
        app.open_explorer_find();
        app.explorer.find.filter = true;
        query(&mut app, "app");
        let index = app
            .explorer
            .entries
            .iter()
            .position(|e| e.name == "app_input.rs")
            .unwrap();
        app.set_explorer_single_selection(index);
        app.open_explorer_context_menu(4, 8);
        let action_index = app
            .explorer_context_menu_items()
            .iter()
            .position(|&label| label == action)
            .unwrap();
        app.execute_explorer_context(action_index);
        wait(&mut app);
        assert!(app.rename.active);
        assert!(!app.explorer.find.open);
        assert!(
            app.explorer
                .entries
                .iter()
                .any(|e| Some(&e.path) == app.rename.target.as_ref())
        );
        app.rename.query = "created".into();
        app.execute_rename();
        assert!(dir.path().join("src/deep/created").exists());
    }
}

#[test]
fn filtered_rename_and_refresh_preserve_file_actions_and_multi_selection() {
    let (dir, mut app) = fixture();
    app.open_explorer_find();
    app.explorer.find.filter = true;
    query(&mut app, "app");
    let first = app
        .explorer
        .entries
        .iter()
        .position(|e| e.name == "app_config.rs")
        .unwrap();
    let second = app
        .explorer
        .entries
        .iter()
        .position(|e| e.name == "app_input.rs")
        .unwrap();
    app.set_explorer_single_selection(first);
    app.toggle_explorer_selection(second);
    let selected = app.explorer_multi_selected.clone();
    app.refresh_explorer_async();
    wait(&mut app);
    assert_eq!(app.explorer_multi_selected, selected);
    app.set_explorer_single_selection(second);
    app.open_explorer_context_menu(4, 8);
    let rename = app
        .explorer_context_menu_items()
        .iter()
        .position(|&label| label == "Rename")
        .unwrap();
    app.execute_explorer_context(rename);
    assert!(app.rename.active);
    for ch in "renamed".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    wait(&mut app);
    assert!(dir.path().join("src/deep/renamed.rs").is_file());
    assert!(
        !app.explorer
            .entries
            .iter()
            .any(|e| e.name == "app_input.rs" || e.name == "renamed.rs")
    );
    app.close_explorer_find();
    wait(&mut app);
    app.explorer
        .reveal_file_deferred(&dir.path().join("src/deep/renamed.rs"));
    app.refresh_explorer_tree_async();
    wait(&mut app);
    assert_eq!(
        app.explorer.entries[app.explorer.selected].name,
        "renamed.rs"
    );
}
