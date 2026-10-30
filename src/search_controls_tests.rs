use super::*;
use crate::app::{ActivePane, GlobalSearchResult, SidebarView, Tab};
use crate::editor::Editor;
use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, style::Style};
use std::time::Duration;

fn fixture() -> (tempfile::TempDir, App, Terminal<TestBackend>) {
    let workspace = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().to_path_buf());
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("foo foo foo")));
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Search;
    app.global_search_active = true;
    app.active_pane = ActivePane::Explorer;
    (
        workspace,
        app,
        Terminal::new(TestBackend::new(120, 30)).unwrap(),
    )
}

#[test]
fn search_bar_uses_editor_widget_and_input_colors() {
    use ratatui::style::Color::Rgb;
    let (_workspace, mut app, mut terminal) = fixture();
    app.search_query = "theme".into();
    app.search_input_focused = false;
    let mut theme = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles();
    for (key, value) in [("editorWidget.background", "#0d1016"),
        ("input.background", "#1e2832"), ("input.foreground", "#b3b1ad"),
        ("editorWidget.border", "#3c4650")] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    app.colors = theme.to_theme_colors();
    terminal.draw(|frame| crate::ui_search_dialog::draw_search_bar(frame, &mut app)).unwrap();
    let area = app.search_bar_area.unwrap();
    let position = label_position(&terminal, area, "theme");
    let cell = &terminal.backend().buffer()[position];
    assert_eq!(cell.fg, Rgb(179, 177, 173));
    assert_eq!(cell.bg, Rgb(30, 40, 50));
    let border = &terminal.backend().buffer()[(area.x, area.y)];
    assert_eq!(border.fg, Rgb(60, 70, 80));
    assert_eq!(border.bg, Rgb(13, 16, 22));
}

#[test]
fn find_widget_layers_fill_text_inputs_and_controls_on_the_visible_editor_surface() {
    use crate::theme_workbench::ColorLayer;
    use ratatui::{style::{Color, Modifier, Style}, widgets::Paragraph};
    let (_workspace, mut app, mut terminal) = fixture();
    app.search_query = "query".into();
    app.search_input_focused = false;
    app.search_error = Some("Invalid expression".into());
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    for (fill, owner, expected_bg) in [
        ("#28506480", Color::Rgb(20, 40, 60), Color::Rgb(30, 60, 80)),
        ("#28506480", Color::Rgb(50, 10, 65), Color::Rgb(45, 45, 83)),
        ("#28506400", Color::Rgb(20, 40, 60), Color::Rgb(20, 40, 60)),
    ] {
        let mut theme = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles();
        for (id, value) in [
            ("editorWidget.background", fill),
            ("editorWidget.foreground", "#ffeedde0"),
            ("editorWidget.border", "#aaddff80"),
            ("input.background", "#6080a040"),
            ("toolbar.hoverBackground", "#80908060"),
            ("errorForeground", "#ffccbbee"),
        ] { theme.workbench_colors.insert(id.into(), value.into()); }
        app.colors = theme.to_theme_colors();
        // A gradient or highlighted editor row can differ from editor.background.
        // Underlying syntax and its modifiers must still be cleared by the widget.
        terminal.draw(|frame| {
            frame.render_widget(Paragraph::new("Editor text\nSyntax text")
                .style(Style::default().bg(owner).add_modifier(Modifier::ITALIC)), frame.area());
            crate::ui_search_dialog::draw_search_bar(frame, &mut app);
        }).unwrap();
        let area = app.search_bar_area.unwrap();
        let buffer = terminal.backend().buffer();
        let title = &buffer[label_position(&terminal, area, "Find")];
        assert_eq!(title.bg, expected_bg, "{fill}, owner={owner:?}");
        assert_eq!(title.fg, ColorLayer::rgba(255, 238, 221, 224).over(expected_bg).unwrap());
        assert!(!title.modifier.contains(Modifier::ITALIC));
        assert_eq!(buffer[(area.x, area.y)].fg, ColorLayer::rgba(170, 221, 255, 128).over(expected_bg).unwrap());
        let query = &buffer[label_position(&terminal, area, "query")];
        assert_eq!(query.bg, ColorLayer::rgba(96, 128, 160, 64).over(expected_bg).unwrap());
        let close = &buffer[label_position(&terminal, area, "×")];
        assert_eq!((close.fg, close.bg), (title.fg, expected_bg));
        let error = &buffer[label_position(&terminal, area, "Invalid expression")];
        assert_eq!((error.fg, error.bg), (ColorLayer::rgba(255, 204, 187, 238).over(expected_bg).unwrap(), expected_bg));
        crate::ui_control_visibility_tests::snapshot(buffer, &format!("find widget {fill} on {owner:?}"));
    }
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16)) {
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

fn label_position(terminal: &Terminal<TestBackend>, area: Rect, label: &str) -> (u16, u16) {
    let width = label.chars().count() as u16;
    let buffer = terminal.backend().buffer();
    (area.y..area.bottom())
        .find_map(|row| {
            (area.x..=area.right().saturating_sub(width)).find_map(|col| {
                let text: String = (col..col + width)
                    .map(|x| buffer[(x, row)].symbol())
                    .collect();
                (text == label).then_some((col, row))
            })
        })
        .unwrap_or_else(|| panic!("visible control {label}"))
}

fn click_label(
    app: &mut App,
    terminal: &mut Terminal<TestBackend>,
    scope: SearchScope,
    label: &str,
) {
    let area = match scope {
        SearchScope::File => app.search_bar_area.unwrap(),
        SearchScope::Workspace => app.explorer_area.unwrap(),
    };
    let position = label_position(terminal, area, label);
    mouse(app, MouseEventKind::Down(MouseButton::Left), position);
    mouse(app, MouseEventKind::Up(MouseButton::Left), position);
    draw(app, terminal);
}

fn has_text(terminal: &Terminal<TestBackend>, text: &str) -> bool {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height).any(|row| {
        (0..buffer.area.width)
            .map(|col| buffer[(col, row)].symbol())
            .collect::<String>()
            .contains(text)
    })
}

#[test]
fn regex_search_controls_switch_modes_and_show_invalid_pattern_errors() {
    let (workspace, mut app, mut terminal) = fixture();
    std::fs::write(workspace.path().join("fixture.txt"), "a*b ab").unwrap();
    app.tabs[app.active_tab] = Tab::new_file(Editor::test_with_text("a*b ab"));
    app.active_pane = ActivePane::Editor;
    app.global_search_active = false;
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('*'), KeyModifiers::NONE);
    assert_eq!(app.search_matches, vec![(0, 1, 2)], "literal mode");
    draw(&mut app, &mut terminal);
    click_label(&mut app, &mut terminal, SearchScope::File, ".*");
    assert!(app.search_regex && app.search_matches.is_empty());
    assert!(has_text(&terminal, "Invalid regex:"));
    assert!(!has_text(&terminal, "No results"));
    key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('\\'), KeyModifiers::NONE);
    draw(&mut app, &mut terminal);
    assert_eq!(app.search_query, r"\*");
    assert_eq!(app.search_matches, vec![(0, 1, 2)]);
    assert!(app.search_error.is_none());
    assert!(!has_text(&terminal, "Invalid regex:"));

    app.global_search_regex = true;
    app.global_search_query = "(?=a)".into();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    draw(&mut app, &mut terminal);
    assert!(app.global_search_error.is_none(), "VS Code accepts JavaScript lookarounds");
    assert!(!app.global_search_results.is_empty());
    app.global_search_query = "(?s).*".into();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    draw(&mut app, &mut terminal);
    assert!(app.global_search_error.is_some(), "inline Rust regex flags are invalid JavaScript");
    assert!(has_text(&terminal, "Invalid regex:"));
    app.global_search_query = r"\*".into();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    draw(&mut app, &mut terminal);
    assert_eq!(app.global_search_results.len(), 1);
    assert!(app.global_search_error.is_none());
    assert!(!has_text(&terminal, "Invalid regex:"));
}

#[test]
fn find_bar_covers_underlying_text_and_formatting() {
    use ratatui::style::{Color, Modifier};

    let (_workspace, mut app, _) = fixture();
    app.search_active = true;
    app.search_input_focused = true;
    for width in [40, 60, 120] {
        for replace in [false, true] {
            for query in ["", "foo", "a long search query with Unicode: 日本語"] {
                app.replace_mode = replace;
                app.search_query = query.into();
                app.search_cursor = query.len();
                app.replace_query = "replacement".into();
                app.replace_cursor = app.replace_query.len();
                app.update_search();
                let mut reference = Terminal::new(TestBackend::new(width, 30)).unwrap();
                reference
                    .draw(|frame| crate::ui_search_dialog::draw_search_bar(frame, &mut app))
                    .unwrap();
                let bar = app.search_bar_area.unwrap();
                let close = app
                    .search_control_regions
                    .iter()
                    .find(|button| button.control == SearchControl::Close)
                    .unwrap()
                    .area;
                let close_label: String = (close.x..close.right())
                    .map(|col| reference.backend().buffer()[(col, close.y)].symbol())
                    .collect();
                assert_eq!(
                    close_label, " × ",
                    "Close button must clear the border behind it"
                );

                let mut over_content = Terminal::new(TestBackend::new(width, 30)).unwrap();
                over_content
                    .draw(|frame| {
                        // Every gap in the popup covers a styled character, just as it
                        // can when Find is opened over Markdown or selected code.
                        let background = "underlying editor content ".repeat(width as usize);
                        let style = Style::default()
                            .fg(Color::Red)
                            .bg(Color::Blue)
                            .add_modifier(Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED);
                        for row in 0..frame.area().height {
                            frame.render_widget(
                                Paragraph::new(background.as_str()).style(style),
                                Rect::new(0, row, width, 1),
                            );
                        }
                        crate::ui_search_dialog::draw_search_bar(frame, &mut app);
                    })
                    .unwrap();

                for row in bar.y..bar.bottom() {
                    for col in bar.x..bar.right() {
                        assert_eq!(
                            over_content.backend().buffer()[(col, row)],
                            reference.backend().buffer()[(col, row)],
                            "Find popup at {col},{row}, width {width}, replace {replace}, query {query:?}"
                        );
                    }
                }
                assert_eq!(over_content.backend().buffer()[(0, 0)].symbol(), "u");
                assert_eq!(over_content.backend().buffer()[(0, 0)].fg, Color::Red);
            }
        }
    }
}

#[test]
fn search_sidebar_replace_toggle_preserves_fields_and_skips_hidden_input() {
    for right in [false, true] {
        let (_workspace, mut app, mut terminal) = fixture();
        app.sidebar_right = right;
        app.global_search_query = "needle".into();
        app.global_search_replace_query = "replacement".into();
        app.search_focus_field = 1;
        draw(&mut app, &mut terminal);
        let results_y =
            crate::ui_search_panel::search_results_start_y(&app, app.explorer_area.unwrap());
        click_label(&mut app, &mut terminal, SearchScope::Workspace, "▾");
        assert!(!app.global_search_replace_visible);
        assert_eq!(app.search_focus_field, 0);
        assert_eq!(app.text_input_regions.len(), 3);
        assert_eq!(
            crate::ui_search_panel::search_results_start_y(&app, app.explorer_area.unwrap()),
            results_y - 1
        );
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.search_focus_field, 2);
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.search_focus_field, 0);
        click_label(&mut app, &mut terminal, SearchScope::Workspace, "▸");
        assert!(app.global_search_replace_visible);
        assert_eq!(app.search_focus_field, 1);
        assert_eq!(app.global_search_query, "needle");
        assert_eq!(app.global_search_replace_query, "replacement");
        assert_eq!(app.text_input_regions.len(), 4);
    }
}

#[test]
fn file_find_does_not_reopen_after_the_last_editor_closes() {
    let (_workspace, mut app, mut terminal) = fixture();
    app.global_search_active = false;
    app.active_pane = ActivePane::Editor;
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    draw(&mut app, &mut terminal);
    assert!(app.search_bar_area.is_some());
    app.force_close_tab(0);
    assert!(app.tabs.is_empty());

    let configured_keybindings = app.keybindings.clone();
    for pane in [ActivePane::Editor, ActivePane::Explorer] {
        app.active_pane = pane;
        for fallback in [false, true] {
            app.keybindings = if fallback { Vec::new() } else { configured_keybindings.clone() };
            for code in [KeyCode::Char('f'), KeyCode::Char('h')] {
                key(&mut app, code, KeyModifiers::CONTROL);
                draw(&mut app, &mut terminal);
                assert!(!app.search_active && !app.search_input_focused);
                assert!(app.search_bar_area.is_none());
            }
        }
        for command in ["find", "find_replace"] {
            app.execute_palette_command(command);
            draw(&mut app, &mut terminal);
            assert!(!app.search_active && !app.search_input_focused);
            assert!(app.search_bar_area.is_none());
        }
    }

    // Workspace search remains available with no editor open.
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
    assert!(app.global_search_active);
    assert_eq!(app.sidebar_view, SidebarView::Search);
}

#[test]
fn find_bar_resizes_horizontally_and_keeps_inputs_and_controls_working() {
    let (_workspace, mut app, mut terminal) = fixture();
    app.global_search_active = false;
    app.active_pane = ActivePane::Editor;
    app.execute_palette_command("find_replace");
    app.search_query = "find-this-long-search-term-across-the-whole-file".into();
    app.replace_query = "replace-this-with-another-long-value-in-the-file".into();
    app.search_cursor = app.search_query.len();
    app.replace_cursor = app.replace_query.len();
    app.search_selection_anchor = Some(0);
    draw(&mut app, &mut terminal);
    let original = app.search_bar_area.unwrap();
    assert!(!has_text(&terminal, &app.search_query));
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), "find bar default width");
    let editor_cursor = app.editor().map(|ed| (ed.cursor_line, ed.cursor_col, ed.selection_anchor));

    mouse(&mut app, MouseEventKind::Moved, (original.x, original.y + 1));
    draw(&mut app, &mut terminal);
    assert!(has_text(&terminal, "Drag left or right to resize"));
    app.mouse_hover_changed = false;
    mouse(&mut app, MouseEventKind::Moved, (original.x - 1, original.y + 1));
    assert!(app.mouse_hover_changed, "leaving the resize handle clears its highlight");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (original.x, original.y + 1));
    assert!(app.search_bar_layout.resizing);
    // Drag outside the widget's rows and across workbench panes.
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (original.x - 35, 20));
    draw(&mut app, &mut terminal);
    let wider = app.search_bar_area.unwrap();
    assert_eq!(wider.width, (original.width + 35).min(app.viewport.unwrap().width - 2));
    assert_eq!(wider.right(), original.right());
    assert!(has_text(&terminal, &app.search_query));
    assert!(has_text(&terminal, &app.replace_query));
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), "find bar expanded width");
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (wider.x, 20));
    assert!(!app.search_bar_layout.resizing);
    assert_eq!(app.search_selection_anchor, Some(0));
    assert_eq!(app.editor().map(|ed| (ed.cursor_line, ed.cursor_col, ed.selection_anchor)), editor_cursor);
    assert!(!app.resizing_explorer && !app.resizing_panel);
    assert!(app.drag_start_pos.is_none());

    // Dragging right narrows the same edge, even over the existing input.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (wider.x, wider.y + 1));
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (wider.x + 10, wider.y + 1));
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (wider.x + 10, wider.y + 1));
    draw(&mut app, &mut terminal);
    let width = app.search_bar_area.unwrap().width;
    assert_eq!(width, wider.width - 10);
    assert_eq!(app.search_selection_anchor, Some(0));

    click_label(&mut app, &mut terminal, SearchScope::File, "Aa");
    assert!(app.search_case_sensitive);
    click_label(&mut app, &mut terminal, SearchScope::File, "×");
    assert!(!app.search_active);
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    draw(&mut app, &mut terminal);
    assert_eq!(app.search_bar_area.unwrap().width, width);
    click_label(&mut app, &mut terminal, SearchScope::File, "▸");
    assert!(app.replace_mode);
}

#[test]
fn find_bar_resize_limits_follow_the_viewport_and_keep_the_preferred_width() {
    let (_workspace, mut app, mut terminal) = fixture();
    app.global_search_active = false;
    app.active_pane = ActivePane::Editor;
    app.execute_palette_command("find");
    draw(&mut app, &mut terminal);
    let max_width = app.viewport.unwrap().width - 2;
    for (col, expected_width) in [(u16::MAX, 40.min(max_width)), (0, max_width)] {
        let bar = app.search_bar_area.unwrap();
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (bar.x, bar.y + 1));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (col, 20));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (col, 20));
        draw(&mut app, &mut terminal);
        assert_eq!(app.search_bar_area.unwrap().width, expected_width);
    }
    terminal = Terminal::new(TestBackend::new(30, 16)).unwrap();
    draw(&mut app, &mut terminal);
    let viewport = app.viewport.unwrap();
    if let Some(small) = app.search_bar_area {
        assert_eq!(small.width, viewport.width - 2);
        assert_eq!(small.x, viewport.x + 1);
        assert_eq!(small.intersection(viewport), small);
    } else {
        assert!(viewport.width < 14 || viewport.height < 3);
    }
    assert_eq!(app.search_bar_layout.width, max_width);
    terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    draw(&mut app, &mut terminal);
    assert_eq!(app.search_bar_area.unwrap().width, max_width);

    let bar = app.search_bar_area.unwrap();
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (bar.x, bar.y + 1));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.search_bar_layout.resizing);
    assert!(!app.search_active);
}

#[test]
fn ctrl_f_exposes_replace_buttons_and_replace_all_is_undoable() {
    let (_workspace, mut app, mut terminal) = fixture();
    app.global_search_active = false;
    app.active_pane = ActivePane::Editor;
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    for ch in "foo".chars() {
        key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
    }
    draw(&mut app, &mut terminal);
    assert!(app.search_active);
    assert!(!app.replace_mode);
    click_label(&mut app, &mut terminal, SearchScope::File, "▸");
    assert!(app.replace_mode && app.search_focus_replace);
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    draw(&mut app, &mut terminal);
    click_label(&mut app, &mut terminal, SearchScope::File, "↵");
    assert_eq!(app.editor().unwrap().rope.to_string(), "x foo foo");
    click_label(&mut app, &mut terminal, SearchScope::File, "▾");
    assert!(!app.replace_mode && !app.search_focus_replace);
    key(&mut app, KeyCode::Char('h'), KeyModifiers::CONTROL);
    assert!(app.replace_mode);
    if !app.search_focus_replace {
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    }
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
    draw(&mut app, &mut terminal);
    assert!(app.replace_query.is_empty());
    click_label(&mut app, &mut terminal, SearchScope::File, "⇆");
    assert_eq!(app.editor().unwrap().rope.to_string(), "x  ");
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope.to_string(), "x foo foo");
}

#[test]
fn every_search_button_has_a_tooltip_at_its_rendered_position() {
    for (scope, right, width) in [
        (SearchScope::Workspace, false, 120),
        (SearchScope::Workspace, true, 120),
        (SearchScope::File, false, 120),
        (SearchScope::File, false, 60),
    ] {
        let (workspace, mut app, _) = fixture();
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        app.sidebar_right = right;
        app.global_search_query = "foo".into();
        app.global_search_results = vec![GlobalSearchResult {
            file_path: workspace.path().join("file.txt"),
            line_num: 1,
            line_text: "foo".into(),
            col_start: 0,
            col_end: 3,
            context_before: None,
            context_after: None,
        }].into();
        if scope == SearchScope::File {
            app.search_active = true;
            app.replace_mode = true;
            app.search_query = "foo".into();
            app.update_search();
        }
        let mut labels = vec![
            ("▾", "Hide Replace"),
            ("Aa", "Match Case"),
            ("ab", "Match Whole Word"),
            (".*", "Regular Expression"),
            ("AB", "Preserve Case"),
            ("⇆", "Replace All"),
        ];
        if scope == SearchScope::File {
            labels.extend([
                ("⊞", "Find in Selection"),
                ("↑", "Previous Match"),
                ("↓", "Next Match"),
                ("↵", "Replace (Enter)"),
                ("×", "Close (Escape)"),
            ]);
        } else {
            labels.extend([
                ("↻", "Refresh Search"),
                ("×", "Clear Search Results"),
                ("⊟", "Collapse All Results"),
            ]);
        }
        for (label, tooltip) in labels {
            mouse(&mut app, MouseEventKind::Moved, (0, 0));
            draw(&mut app, &mut terminal);
            let area = if scope == SearchScope::File {
                app.search_bar_area.unwrap()
            } else {
                app.explorer_area.unwrap()
            };
            let position = label_position(&terminal, area, label);
            mouse(&mut app, MouseEventKind::Moved, position);
            draw(&mut app, &mut terminal);
            assert!(
                has_text(&terminal, tooltip),
                "tooltip for {label} in {scope:?}, width={width}"
            );
            assert_eq!(
                label_position(&terminal, area, label),
                position,
                "tooltip must leave its control visible"
            );
        }
        mouse(&mut app, MouseEventKind::Moved, (0, 0));
        draw(&mut app, &mut terminal);
        assert!(!has_text(&terminal, "Alt+C"));
    }
}

#[test]
fn workspace_search_stop_button_keeps_partial_results_and_refresh_restarts_search() {
    for right in [false, true] {
        for partial_results in [false, true] {
            let (workspace, mut app, mut terminal) = fixture();
            app.sidebar_right = right;
            let path = workspace.path().join("file.txt");
            std::fs::write(&path, "foo foo\nbar foo\n").unwrap();
            app.global_search_query = "foo".into();
            if partial_results {
                app.global_search_results = vec![GlobalSearchResult {
                    file_path: path.clone(),
                    line_num: 1,
                    line_text: "foo foo".into(),
                    col_start: 0,
                    col_end: 3,
                    context_before: None,
                    context_after: None,
                }].into();
            }
            let token = crate::background::CancellationToken::new();
            app.search_cancel_token = Some(token.clone());
            app.bg_global_search_running = true;
            draw(&mut app, &mut terminal);
            let area = app.explorer_area.unwrap();
            let stop = label_position(&terminal, area, "■");
            mouse(&mut app, MouseEventKind::Moved, stop);
            draw(&mut app, &mut terminal);
            assert!(has_text(&terminal, "Stop Search (Escape)"));

            click_label(&mut app, &mut terminal, SearchScope::Workspace, "■");
            assert!(token.is_cancelled());
            assert!(!app.bg_global_search_running);
            assert!(app.global_search_active);
            assert_eq!(app.global_search_query, "foo");
            assert_eq!(app.global_search_results.len(), usize::from(partial_results));
            assert!(has_text(&terminal, if partial_results { "(stopped)" } else { "Search stopped" }));
            assert!(!app.search_control_regions.iter().any(|button| button.control == SearchControl::Stop));

            click_label(&mut app, &mut terminal, SearchScope::Workspace, "↻");
            assert!(app.bg_global_search_running);
            assert!(!app.global_search_stopped);
            app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
            draw(&mut app, &mut terminal);
            assert_eq!(app.global_search_results.len(), 3);
            assert!(!has_text(&terminal, "stopped"));
        }
    }
}

#[test]
fn escape_stops_workspace_search_before_releasing_search_focus() {
    let (_workspace, mut app, mut terminal) = fixture();
    app.global_search_query = "foo".into();
    let token = crate::background::CancellationToken::new();
    app.search_cancel_token = Some(token.clone());
    app.bg_global_search_running = true;

    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(token.is_cancelled());
    assert!(!app.bg_global_search_running);
    assert!(app.global_search_active);
    assert_eq!(app.global_search_query, "foo");
    draw(&mut app, &mut terminal);
    assert!(has_text(&terminal, "Search stopped"));

    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.global_search_active);
    assert_eq!(app.global_search_query, "foo");
}

#[test]
fn workspace_replace_all_updates_open_and_closed_files_and_refreshes_matches() {
    for replacement in ["bar", ""] {
        let (workspace, mut app, mut terminal) = fixture();
        let open_path = workspace.path().join("open.txt");
        let closed_path = workspace.path().join("closed.txt");
        for path in [&open_path, &closed_path] {
            std::fs::write(path, "foo foo\n").unwrap();
        }
        app.open_file(&open_path);
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.editor().unwrap().bg_loading
        });
        let editor = app.editor_mut().unwrap();
        editor.rope = ropey::Rope::from_str("unsaved\nfoo foo\n");
        editor.modified = true;
        app.global_search_query = "foo".into();
        app.update_global_search();
        app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert_eq!(app.global_search_results.len(), 4);
        app.active_pane = ActivePane::Explorer;
        app.search_focus_field = 1;
        let generation = app.search_generation;
        for ch in replacement.chars() {
            key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
        }
        assert_eq!(
            app.search_generation, generation,
            "editing replacement text must retain the results"
        );
        draw(&mut app, &mut terminal);
        click_label(&mut app, &mut terminal, SearchScope::Workspace, "⇆");
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.bg_global_replace_running && !app.bg_global_search_running
        });
        let expected = format!("{replacement} {replacement}\n");
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            format!("unsaved\n{expected}")
        );
        assert_eq!(std::fs::read_to_string(&closed_path).unwrap(), expected);
        assert_eq!(
            std::fs::read_to_string(&open_path).unwrap(),
            "foo foo\n",
            "open editors retain unsaved changes"
        );
        assert!(app.editor().unwrap().modified);
        assert!(
            app.global_search_results.is_empty(),
            "results must reflect the replaced open buffer"
        );
    }
}

#[test]
fn clicking_search_activity_focuses_query_from_editor() {
    for (visible, view) in [
        (false, SidebarView::Explorer),
        (true, SidebarView::Explorer),
        (true, SidebarView::Search),
    ] {
        let (_workspace, mut app, mut terminal) = fixture();
        app.show_explorer = visible;
        app.sidebar_view = view;
        app.global_search_active = visible && view == SidebarView::Search;
        app.active_pane = ActivePane::Editor;
        app.terminal_panel_focused = true;
        app.search_active = true;
        app.search_input_focused = true;
        app.search_query = "editor find".into();
        app.global_search_query = "find".into();
        app.global_search_field_cursors[0] = 4;
        app.search_focus_field = 3;
        draw(&mut app, &mut terminal);
        let search = label_position(&terminal, app.activity_bar_area.unwrap(), "SEAR");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), search);
        assert!(app.show_explorer);
        assert_eq!(app.sidebar_view, SidebarView::Search);
        assert_eq!(app.active_pane, ActivePane::Explorer);
        assert!(app.global_search_active);
        assert_eq!(app.search_focus_field, 0);
        assert!(!app.search_input_focused);
        assert!(!app.terminal_panel_focused);
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.global_search_query, "findx");
        assert_eq!(app.search_query, "editor find");
        assert_eq!(app.editor().unwrap().rope.to_string(), "foo foo foo");
    }
}

#[test]
fn clicking_search_result_navigates_on_first_click() {
    for already_open in [false, true] {
        let (workspace, mut app, mut terminal) = fixture();
        let path = workspace.path().join("result.txt");
        let line = format!("é界{}needle value", " ".repeat(90));
        let content = format!("{}{line}\n", "earlier line\n".repeat(70));
        std::fs::write(&path, &content).unwrap();
        if already_open {
            app.open_file(&path);
            app.wait_for_background(Duration::from_secs(5), |app| {
                !app.editor().unwrap().bg_loading
            });
        }
        app.global_search_query = "needle".into();
        app.global_search_results = vec![GlobalSearchResult {
            file_path: path.clone(),
            line_num: 71,
            line_text: line.into(),
            col_start: 92,
            col_end: 98,
            context_before: None,
            context_after: None,
        }].into();
        draw(&mut app, &mut terminal);
        let panel = app.explorer_area.unwrap();
        let result_y = crate::ui_search_panel::search_results_start_y(&app, panel) + 1;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (panel.x + 3, result_y),
        );
        assert_eq!(app.active_pane, ActivePane::Editor);
        if !already_open {
            assert!(app.editor().unwrap().bg_loading);
        }
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.editor().unwrap().bg_loading
        });
        draw(&mut app, &mut terminal);
        let editor = app.editor().unwrap();
        assert_eq!(editor.file_path.as_ref(), Some(&path));
        assert_eq!((editor.cursor_line, editor.cursor_col), (70, 92));
        assert!(editor.scroll_offset > 0, "match must be revealed vertically");
        assert!(editor.horizontal_scroll > 0, "match must be revealed horizontally");
        key(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
        assert_eq!(app.global_search_query, "needle");
        assert!(app.editor().unwrap().rope.line(70).to_string().contains("!needle"));
    }
}

#[test]
fn search_shortcut_and_command_focus_query_from_editor() {
    for command in [false, true] {
        let (_workspace, mut app, _) = fixture();
        app.active_pane = ActivePane::Editor;
        app.search_input_focused = true;
        app.terminal_panel_focused = true;
        app.search_focus_field = 3;
        if command {
            app.execute_palette_command("global_search");
        } else {
            key(&mut app, KeyCode::Char('F'), KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        }
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.active_pane, ActivePane::Explorer);
        assert_eq!(app.search_focus_field, 0);
        assert_eq!(app.global_search_query, "x");
        assert!(!app.search_input_focused);
        assert!(!app.terminal_panel_focused);
        assert_eq!(app.editor().unwrap().rope.to_string(), "foo foo foo");
    }
}

#[test]
fn clicking_search_activity_refocuses_other_fields_then_toggles_focused_query() {
    let (_workspace, mut app, mut terminal) = fixture();
    app.search_focus_field = 3;
    draw(&mut app, &mut terminal);
    let search = label_position(&terminal, app.activity_bar_area.unwrap(), "SEAR");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), search);
    assert!(app.show_explorer);
    assert_eq!(app.search_focus_field, 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), search);
    assert!(!app.show_explorer);
    assert!(!app.global_search_active);
    assert_eq!(app.active_pane, ActivePane::Editor);
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.editor().unwrap().rope.to_string(), "xfoo foo foo");
    assert!(app.global_search_query.is_empty());
}

#[test]
fn latest_search_click_survives_loading_in_an_inactive_split() {
    for restoring_session in [false, true] {
        let (workspace, mut app, mut terminal) = fixture();
        let path = workspace.path().join("loading.txt");
        let contents = "prefix needle\n".repeat(60);
        // Session completion probes the disk. A synthetic successful load of
        // a nonexistent file races that probe, which correctly closes the tab.
        std::fs::write(&path, &contents).unwrap();
        // Keep completion under test control so both clicks happen while loading.
        let mut editor = Editor::new_loading(&path, 0);
        let session_restore = restoring_session.then(|| crate::session::SessionRestore {
            id: 123,
            tab: serde_json::from_value(serde_json::json!({
                "filePath": path.to_string_lossy(),
                "cursorLine": 1,
                "cursorCol": 2,
                "scrollOffset": 0,
                "horizontalScroll": 0,
                "pinned": false
            })).unwrap(),
        });
        editor.pending_session_restore = session_restore.clone();
        app.tabs.push(Tab::new_file(editor));
        app.global_search_query = "needle".into();
        app.global_search_results = [3, 50].into_iter().map(|line_num| GlobalSearchResult {
            file_path: path.clone(),
            line_num,
            line_text: "prefix needle".into(),
            col_start: 7,
            col_end: 13,
            context_before: None,
            context_after: None,
        }).collect();
        for index in 0..2 {
            draw(&mut app, &mut terminal);
            let panel = app.explorer_area.unwrap();
            let result_y = crate::ui_search_panel::search_results_start_y(&app, panel) + 1 + index;
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (panel.x + 3, result_y));
            assert!(app.editor().unwrap().bg_loading);
        }
        app.create_split_group(app.active_tab);
        let loading_group = app.active_group;
        app.switch_group(0);
        let rope = ropey::Rope::from_str(&contents);
        app.background.sender.send(crate::background::BackgroundResult::FileLoaded {
            workspace_generation: 0,
            path,
            file_size: rope.len_bytes() as u64,
            rope,
            encoding: "UTF-8",
            last_disk_modified: None,
            session_restores: session_restore.into_iter().collect(),
            skip_open_prompt: true,
        }).unwrap();
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
        assert_eq!(app.active_group, 0, "completion must not steal focus");
        assert_eq!(app.editor().unwrap().cursor_line, 0);
        app.switch_group(loading_group);
        assert!(!app.editor().unwrap().bg_loading);
        assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (49, 7));
        assert!(app.editor().unwrap().pending_cursor_position.is_none());
    }
}

#[test]
fn clicking_between_find_and_workspace_search_changes_keyboard_focus() {
    let (_workspace, mut app, mut terminal) = fixture();
    app.search_active = true;
    app.search_input_focused = true;
    app.search_query = "foo".into();
    draw(&mut app, &mut terminal);
    let panel = app.explorer_area.unwrap();
    let search_field = label_position(
        &terminal,
        Rect::new(panel.x, panel.y + 1, panel.width, 1),
        "Search",
    );
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        search_field,
    );
    key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "a");
    assert_eq!(app.search_query, "foo");
    draw(&mut app, &mut terminal);
    let find_field = label_position(&terminal, app.search_bar_area.unwrap(), "foo");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        find_field,
    );
    key(&mut app, KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(app.search_query, "bfoo");
    assert_eq!(app.global_search_query, "a");
}

#[test]
fn asynchronous_replace_result_applies_in_large_file_mode_and_rejects_stale_edits() {
    for (large_file, stale) in [(false, false), (true, false), (true, true)] {
        let (_workspace, mut app, _) = fixture();
        app.editor_mut().unwrap().large_file_mode = large_file;
        let target = app.editor_content_target().unwrap();
        let source_document_id = app.editor().unwrap().save_identity;
        let prepared = crate::formatters::prepare(&app.editor().unwrap().rope, "bar bar bar");
        if stale {
            app.editor_mut().unwrap().insert_char('!');
        }
        let before = app.editor().unwrap().rope.to_string();
        app.bg_file_replace_running = true;
        app.background
            .sender
            .send(crate::background::BackgroundResult::FileReplaceDone {
                generation: app.file_replace_generation,
                target,
                source_document_id,
                prepared,
                replacements: 3,
            })
            .unwrap();
        app.poll_background_results();
        assert!(!app.bg_file_replace_running);
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            if stale {
                before.as_str()
            } else {
                "bar bar bar"
            }
        );
    }
}

#[test]
fn option_shortcuts_only_refresh_the_focused_search() {
    let (workspace, mut app, _) = fixture();
    let path = workspace.path().join("case.txt");
    std::fs::write(&path, "foo Foo foobar").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    app.global_search_query = "foo".into();
    app.search_query = "foo".into();
    app.search_active = true;
    app.update_search();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert_eq!(app.search_matches.len(), 3);
    assert_eq!(app.global_search_results.len(), 3);

    app.active_pane = ActivePane::Explorer;
    app.search_input_focused = false;
    let file_generation = app.file_search_generation;
    for (option, matches) in [('c', 2), ('w', 1)] {
        key(&mut app, KeyCode::Char(option), KeyModifiers::ALT);
        app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert_eq!(app.search_matches.len(), 3);
        assert_eq!(app.file_search_generation, file_generation);
        assert_eq!(app.global_search_results.len(), matches);
    }
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    let workspace_generation = app.search_generation;
    key(&mut app, KeyCode::Char('w'), KeyModifiers::ALT);
    assert_eq!(app.search_matches.len(), 2);
    key(&mut app, KeyCode::Char('c'), KeyModifiers::ALT);
    assert_eq!(app.search_matches.len(), 1);
    assert_eq!(app.global_search_results.len(), 1);
    assert_eq!(app.search_generation, workspace_generation);
}

#[test]
fn opening_file_find_from_workspace_search_focuses_all_field_shortcuts() {
    use crate::text_input::TextInputId;
    for command in [false, true] {
        for replace in [false, true] {
            let (_workspace, mut app, _) = fixture();
            app.global_search_query = "workspace".into();
            app.global_search_field_cursors[0] = app.global_search_query.len();
            app.search_query = "foo".into();
            if command {
                app.execute_palette_command(if replace { "find_replace" } else { "find" });
            } else {
                key(&mut app, KeyCode::Char(if replace { 'h' } else { 'f' }), KeyModifiers::CONTROL);
            }
            assert_eq!(app.active_pane, ActivePane::Editor);
            assert_eq!(app.focused_text_input_id(), Some(TextInputId::SearchFind));
            assert_eq!(app.replace_mode, replace);
            key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            assert_eq!(app.search_selection_anchor, Some(0));
            assert_eq!(app.global_search_field_selection_anchors[0], None);
            key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_eq!(app.search_query, "x");
            assert_eq!(app.global_search_query, "workspace");
            key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert_eq!(app.search_query, "foo");
            assert_eq!(app.global_search_query, "workspace");
            key(&mut app, KeyCode::Char('w'), KeyModifiers::ALT);
            assert!(app.search_whole_word);
            assert!(!app.global_search_whole_word);
        }
    }
}

fn search_options(app: &App, scope: SearchScope) -> [bool; 4] {
    match scope {
        SearchScope::File => [app.search_case_sensitive, app.search_whole_word,
            app.search_regex, app.search_preserve_case],
        SearchScope::Workspace => [app.global_search_case_sensitive, app.global_search_whole_word,
            app.global_search_regex, app.global_search_preserve_case],
    }
}

#[test]
fn option_buttons_keep_file_and_workspace_preferences_independent() {
    for scope in [SearchScope::File, SearchScope::Workspace] {
        let (_workspace, mut app, mut terminal) = fixture();
        app.search_active = true;
        app.replace_mode = true;
        draw(&mut app, &mut terminal);
        for (index, label) in ["Aa", "ab", ".*", "AB"].into_iter().enumerate() {
            click_label(&mut app, &mut terminal, scope, label);
            let other = if scope == SearchScope::File { SearchScope::Workspace } else { SearchScope::File };
            assert_eq!(search_options(&app, other), [false; 4]);
            let expected = std::array::from_fn(|option| option <= index);
            assert_eq!(search_options(&app, scope), expected);
        }
    }
}

#[test]
fn escape_in_file_find_does_not_stop_workspace_search() {
    let (_workspace, mut app, _) = fixture();
    let cancel = crate::background::CancellationToken::new();
    app.search_cancel_token = Some(cancel.clone());
    app.bg_global_search_running = true;
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.search_active);
    assert!(app.global_search_active);
    assert!(app.bg_global_search_running);
    assert!(!cancel.is_cancelled());
}

#[test]
fn quick_open_owns_input_while_both_search_views_remain_visible() {
    use crate::text_input::TextInputId;
    let (_workspace, mut app, _) = fixture();
    app.global_search_query = "workspace".into();
    app.search_query = "foo".into();
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert_eq!(app.focused_text_input_id(), Some(TextInputId::QuickOpen));
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.quick_open_query, "x");
    assert_eq!(app.search_query, "foo");
    assert_eq!(app.global_search_query, "workspace");
}

#[test]
fn workspace_replacement_uses_its_own_matching_and_preserve_case_options() {
    let (workspace, mut app, _) = fixture();
    let open = workspace.path().join("open.txt");
    let closed = workspace.path().join("closed.txt");
    for path in [&open, &closed] {
        std::fs::write(path, "foo FOO foobar").unwrap();
    }
    app.open_file(&open);
    app.wait_for_background(Duration::from_secs(5), |app| !app.editor().unwrap().bg_loading);
    app.search_case_sensitive = true;
    app.search_regex = true;
    app.global_search_whole_word = true;
    app.global_search_preserve_case = true;
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "bar".into();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert_eq!(app.global_search_results.len(), 4);
    app.replace_all_in_files();
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.bg_global_replace_running && !app.bg_global_search_running
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), "bar BAR foobar");
    assert_eq!(std::fs::read_to_string(closed).unwrap(), "bar BAR foobar");
}

#[test]
fn settings_changes_only_refresh_the_corresponding_search() {
    let (workspace, mut app, _) = fixture();
    let path = workspace.path().join("case.txt");
    std::fs::write(&path, "foo Foo foobar").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| !app.editor().unwrap().bg_loading);
    app.search_active = true;
    app.search_query = "foo".into();
    app.global_search_query = "foo".into();
    app.update_search();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    app.toggle_setting("global_search_whole_word");
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert_eq!(app.global_search_results.len(), 2);
    assert_eq!(app.search_matches.len(), 3);
    let generation = app.search_generation;
    app.toggle_setting("search_case_sensitive");
    assert_eq!(app.search_matches.len(), 2);
    assert_eq!(app.global_search_results.len(), 2);
    assert_eq!(app.search_generation, generation);
}

#[test]
fn docked_terminal_focus_prevents_visible_find_from_claiming_text_shortcuts() {
    let (_workspace, mut app, _) = fixture();
    key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    assert!(app.docked_terminal_has_focus());
    assert_eq!(app.focused_text_input_id(), None);
    assert!(app.file_search_visible());
}

fn scrolled_workspace_results(
    sidebar_right: bool, replace_visible: bool, selected_file_expanded: bool,
) -> (tempfile::TempDir, App, Terminal<TestBackend>) {
    let (workspace, mut app, mut terminal) = fixture();
    app.sidebar_right = sidebar_right;
    app.global_search_replace_visible = replace_visible;
    app.global_search_query = "foo".into();
    app.global_search_results = (0..600).flat_map(|file| {
        let path = workspace.path().join(format!("file-{file:04}.rs"));
        (0..4).map(move |line| GlobalSearchResult {
            file_path: path.clone(), line_num: line + 1,
            line_text: format!("foo match {line}").into(), col_start: 0, col_end: 3,
            context_before: None, context_after: None,
        })
    }).collect();
    app.global_search_results.set_all_collapsed(true);
    if selected_file_expanded {
        let path = app.global_search_results[0].file_path.clone();
        app.global_search_results.set_collapsed(&path, false);
    }
    app.global_search_selected = 0;
    app.global_search_scroll = 320;
    app.scroll_speed = 3;
    draw(&mut app, &mut terminal);
    let area = app.explorer_area.unwrap();
    let row = crate::ui_search_panel::search_results_start_y(&app, area) + 2;
    mouse(&mut app, MouseEventKind::ScrollDown, (area.x + 4, row));
    draw(&mut app, &mut terminal);
    assert!(app.global_search_scroll > 320);
    assert_eq!(app.global_search_selected, 0, "wheel scrolling must leave selection alone");
    (workspace, app, terminal)
}

#[test]
fn workspace_search_expansion_preserves_viewport_with_selection_above_it() {
    use crate::search_results::SearchRow;
    for sidebar_right in [false, true] {
        for replace_visible in [false, true] {
            let (_workspace, mut app, mut terminal) = scrolled_workspace_results(sidebar_right, replace_visible, true);
            let scroll = app.global_search_scroll;
            let area = app.explorer_area.unwrap();
            let start_y = crate::ui_search_panel::search_results_start_y(&app, area);
            // Expand several different files while the selected match remains
            // hundreds of rows above the viewport.
            for _ in 0..3 {
                let (row, index) = app.global_search_results.visible_rows(scroll + 3..scroll + 15)
                    .find_map(|(row, hit)| match hit {
                        SearchRow::Header(index) if app.is_global_search_file_collapsed(&app.global_search_results[index].file_path) => Some((row, index)),
                        _ => None,
                    }).unwrap();
                let path = app.global_search_results[index].file_path.clone();
                let screen_y = start_y + (row - scroll) as u16;
                let point = (area.x + 4, screen_y);
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), point);
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), point);
                draw(&mut app, &mut terminal);
                assert!(!app.is_global_search_file_collapsed(&path));
                assert_eq!(app.global_search_scroll, scroll, "expanding {path:?} must not reveal the old selection");
                assert_eq!(app.global_search_selected, 0);
                let label: String = (area.x..area.right())
                    .map(|x| terminal.backend().buffer()[(x, screen_y)].symbol()).collect();
                assert!(label.contains(path.file_name().unwrap().to_str().unwrap()), "the expanded header stays under the pointer: {label}");
            }
            app.global_search_active = true;
            key(&mut app, KeyCode::Down, KeyModifiers::CONTROL);
            draw(&mut app, &mut terminal);
            assert_eq!(app.global_search_selected, 1, "keyboard navigation must still move selection");
            assert!(app.global_search_scroll < scroll, "keyboard navigation still reveals the chosen match");
        }
    }
}

#[test]
fn workspace_search_collapsing_the_only_expanded_file_preserves_viewport() {
    use crate::search_results::SearchRow;
    for sidebar_right in [false, true] {
        for replace_visible in [false, true] {
            let (_workspace, mut app, mut terminal) = scrolled_workspace_results(sidebar_right, replace_visible, false);
            let scroll = app.global_search_scroll;
            let area = app.explorer_area.unwrap();
            let start_y = crate::ui_search_panel::search_results_start_y(&app, area);
            let row = scroll + 4;
            let Some(SearchRow::Header(index)) = app.global_search_results.row_at(row) else { panic!("collapsed file header") };
            let path = app.global_search_results[index].file_path.clone();
            let point = (area.x + 4, start_y + 4);
            for collapsed in [false, true, false, true] {
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), point);
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), point);
                draw(&mut app, &mut terminal);
                assert_eq!(app.is_global_search_file_collapsed(&path), collapsed);
                assert_eq!(app.global_search_scroll, scroll, "toggling the last expanded file must not reset scrolling");
            }
        }
    }
}
