use super::mouse_tests::{draw, fixture, mouse, state};
use super::*;
use crate::{
    app::{ActivePane, NotifyLevel, Tab},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

fn screen(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
fn click(app: &mut App, point: (u16, u16)) {
    mouse(app, MouseEventKind::Down(MouseButton::Left), point);
    mouse(app, MouseEventKind::Up(MouseButton::Left), point);
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}
fn markers(app: &mut App, count: usize) {
    let editor = app.editor().unwrap();
    let name = editor.file_name().to_owned();
    let id = editor.navigation_id;
    app.cached_problems = (0..count)
        .map(|index| {
            (
                name.clone(),
                index + 1,
                "TODO".into(),
                format!("Marker {index:03}"),
                id,
            )
        })
        .collect();
    app.cached_problems_warning_count = count;
    app.bg_problems_analysis_running = true;
}
fn diagnostics(app: &mut App, count: usize) {
    let path = app.editor().unwrap().file_path.clone().unwrap();
    diagnostics_for_path(app, path, count);
}
fn diagnostics_for_path(app: &mut App, path: std::path::PathBuf, count: usize) {
    // Exercise the UI at the published-report boundary; publication/versioning
    // itself has separate LSP tests and needs no child process in these fixtures.
    let items = (0..count)
        .map(|index| crate::lsp::LspDiagnostic {
            line: index,
            col: 4,
            end_line: index,
            end_col: 6,
            severity: lsp_types::DiagnosticSeverity::ERROR,
            message: format!("Diagnostic {index:03}"),
        })
        .collect();
    app.lsp_manager.diagnostics.insert(path, items);
    app.lsp_manager.diagnostics_version += 1;
}

#[test]
fn wheel_and_shift_wheel_on_every_panel_surface_leave_editor_scroll_and_selection_unchanged() {
    for tab in [1, 2, 3] {
        for split in [false, true] {
            let mut app = fixture(tab, true, split);
            markers(&mut app, 60);
            for index in 0..40 {
                app.notify(format!("Notice {index:03}"));
            }
            draw(&mut app);
            app.editor_mut().unwrap().horizontal_scroll = 7;
            app.cached_max_line_width = 2_000;
            let before = state(&app);
            let panel = app.panel_area.unwrap();
            for row in [panel.y, panel.y + 2, panel.bottom() - 1] {
                for kind in [
                    MouseEventKind::ScrollDown,
                    MouseEventKind::ScrollUp,
                    MouseEventKind::ScrollLeft,
                    MouseEventKind::ScrollRight,
                ] {
                    for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                        crate::mouse::handle_mouse(
                            &mut app,
                            MouseEvent {
                                kind,
                                column: panel.x + 5,
                                row,
                                modifiers,
                            },
                        );
                        assert_eq!(
                            state(&app),
                            before,
                            "tab {tab}, {split}, {kind:?}, {modifiers:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn problems_summary_and_blank_columns_are_not_navigation_targets() {
    let mut app = fixture(1, true, false);
    markers(&mut app, 2);
    draw(&mut app);
    let panel = app.panel_area.unwrap();
    let before = state(&app);
    click(&mut app, (panel.x + 5, panel.y + 1));
    assert_eq!(state(&app), before);
    assert_eq!(hovered_problem(&mut app, panel.x + 5, panel.y + 1), None);
    assert_eq!(hovered_problem(&mut app, panel.right(), panel.y + 2), None);
    assert_eq!(hovered_problem(&mut app, panel.x + 5, panel.y + 2), Some(0));
    click(&mut app, (panel.x + 5, panel.y + 3));
    assert_eq!(app.editor().unwrap().cursor_line, 1);
}

#[test]
fn problems_hover_follows_the_pointer_when_rows_scroll_under_it() {
    let mut app = fixture(1, false, false);
    markers(&mut app, 60);
    draw(&mut app);
    let panel = app.panel_area.unwrap();
    let point = (panel.x + 5, panel.y + 6);
    mouse(&mut app, MouseEventKind::Moved, point);
    mouse(&mut app, MouseEventKind::ScrollDown, point);
    let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    for row in panel.y + 1..panel.bottom() {
        assert_eq!(
            terminal.backend().buffer()[(panel.x + 80, row)].bg,
            if row == point.1 {
                app.colors.bg_hover
            } else {
                app.colors.ui.bg_panel
            },
            "only the row under the stationary pointer may be highlighted"
        );
    }
}

#[test]
fn mixed_marker_and_lsp_results_scroll_to_the_end_and_navigate_utf16_locations() {
    let mut app = fixture(1, false, false);
    app.editor_mut().unwrap().rope = ropey::Rope::from_str(&"λ😃 value\n".repeat(100));
    markers(&mut app, 3);
    diagnostics(&mut app, 40);
    let initial = screen(&mut app);
    assert!(!initial.contains("No problems found"));
    assert!(initial.contains("40 errors, 3 warnings"));
    let panel = app.panel_area.unwrap();
    for _ in 0..50 {
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            (panel.x + 5, panel.y + 2),
        );
    }
    let text = screen(&mut app);
    assert!(text.contains("Diagnostic 039"));
    let before_header_click = state(&app);
    // Scrolling moves the virtual headers too, rather than changing click offsets.
    app.problem_scroll = 4;
    draw(&mut app);
    click(&mut app, (panel.x + 5, panel.y + 1));
    assert_eq!(state(&app), before_header_click);
    app.problem_scroll = usize::MAX;
    draw(&mut app);
    click(&mut app, (panel.x + 5, panel.bottom() - 1));
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        (39, 3)
    );
    app.cached_problems.clear();
    diagnostics(&mut app, 0);
    let cleared = screen(&mut app);
    assert_eq!(app.problem_scroll, 0);
    assert!(cleared.contains("Scanning problems"));
    assert!(app.panel_chrome.scrollbar.is_none());
}

#[test]
fn diagnostic_navigation_into_an_unopened_file_survives_background_loading() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("unicode.rs");
    std::fs::write(&path, "λ😃 value\n".repeat(100)).unwrap();
    let mut app = fixture(1, true, false);
    markers(&mut app, 0);
    diagnostics_for_path(&mut app, path.clone(), 1);
    draw(&mut app);
    let before = state(&app);
    let panel = app.panel_area.unwrap();
    click(&mut app, (panel.x + 5, panel.y + 3));
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&path));
    assert!(app.editor().unwrap().bg_loading);
    assert_eq!(app.editor().unwrap().pending_cursor_position, Some((0, 4)));
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        (0, 3)
    );
    assert!(app.editor().unwrap().selection_anchor.is_none());
    app.active_tab = 0;
    assert_eq!(
        state(&app),
        before,
        "opening a diagnostic must preserve its source editor"
    );
}

#[test]
fn marker_navigation_uses_buffer_identity_for_duplicate_basenames_and_untitled_tabs() {
    for untitled in [false, true] {
        let mut app = fixture(1, false, false);
        app.bg_problems_analysis_running = true;
        app.editor_mut().unwrap().file_path =
            (!untitled).then(|| std::path::PathBuf::from("one/main.rs"));
        let mut second = Editor::test_with_text(&"second\n".repeat(100));
        second.file_path = (!untitled).then(|| std::path::PathBuf::from("two/main.rs"));
        let id = second.navigation_id;
        let name = second.file_name().to_owned();
        app.tabs.push(Tab::new_file(second));
        app.cached_problems = vec![(name, 20, "TODO".into(), "Correct buffer".into(), id)];
        draw(&mut app);
        let panel = app.panel_area.unwrap();
        click(&mut app, (panel.x + 4, panel.y + 2));
        assert_eq!(app.editor().unwrap().navigation_id, id);
        assert_eq!(app.editor().unwrap().cursor_line, 19);
        app.close_tab(1);
        let before = state(&app);
        click(&mut app, (panel.x + 4, panel.y + 2));
        assert_eq!(
            state(&app),
            before,
            "a closed result cannot select a same-named tab"
        );
    }
}

#[test]
fn list_scrollbars_keep_exact_grab_offsets_and_do_not_drag_the_editor() {
    for tab in [1, 3] {
        let mut app = fixture(tab, true, false);
        markers(&mut app, 80);
        for index in 0..80 {
            app.notify(format!("Notice {index:03}"));
        }
        draw(&mut app);
        if tab == 1 {
            app.problem_scroll = 7;
        } else {
            app.panel_chrome.notification_top = Some(app.notification_history[7].2);
        }
        draw(&mut app);
        let bar = app.panel_chrome.scrollbar.unwrap();
        let point = (bar.area.x, bar.area.y + bar.thumb_top);
        let before = state(&app);
        click(&mut app, point);
        let offset = if tab == 1 {
            app.problem_scroll
        } else {
            notification_offset(&app, bar.area.height as usize)
        };
        assert_eq!(
            offset, 7,
            "stationary scrollbar press must not quantize the offset"
        );
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), point);
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (bar.area.x, u16::MAX),
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            (bar.area.x, u16::MAX),
        );
        let offset = if tab == 1 {
            app.problem_scroll
        } else {
            notification_offset(&app, bar.area.height as usize)
        };
        assert_eq!(offset, bar.maximum);
        assert_eq!(state(&app), before);
        draw(&mut app);
        let bar = app.panel_chrome.scrollbar.unwrap();
        let point = (bar.area.x, bar.area.y + bar.thumb_top);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), point);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), (0, 0));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (0, 0));
        let offset = if tab == 1 {
            app.problem_scroll
        } else {
            notification_offset(&app, bar.area.height as usize)
        };
        assert_eq!(offset, 0);
        assert_eq!(state(&app), before);
    }
}

#[test]
fn notification_history_stays_readable_during_new_messages_and_retention() {
    let mut app = fixture(3, true, false);
    for index in 0..210 {
        app.notify(format!("Notice {index:03}"));
    }
    assert_eq!(app.notification_history.len(), 200);
    assert!(
        app.notification_history
            .windows(2)
            .all(|pair| pair[0].2 < pair[1].2)
    );
    draw(&mut app);
    let panel = app.panel_area.unwrap();
    mouse(
        &mut app,
        MouseEventKind::ScrollUp,
        (panel.x + 5, panel.y + 2),
    );
    let offset = notification_offset(&app, panel.height.saturating_sub(1) as usize);
    let retained = app.notification_history[offset].0.clone();
    for index in 0..5 {
        app.notify(format!("New {index}"));
    }
    assert!(screen(&mut app).contains(&retained));
    for index in 0..210 {
        app.notify(format!("Retained {index:03}"));
    }
    assert!(screen(&mut app).contains("Retained 010"));
    for _ in 0..100 {
        mouse(
            &mut app,
            MouseEventKind::ScrollDown,
            (panel.x + 5, panel.y + 2),
        );
    }
    assert!(app.panel_chrome.notification_top.is_none());
    app.notify("Latest notification");
    assert!(screen(&mut app).contains("Latest notification"));
    app.notification_history = vec![("Only one".into(), NotifyLevel::Info, 1)];
    draw(&mut app);
    assert!(app.panel_chrome.scrollbar.is_none());
}

#[test]
fn output_workbench_shortcuts_restore_focus_without_waiting_for_a_repaint() {
    let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    for action in ["problems", "zen", "maximize", "search", "explorer"] {
        let mut app = fixture(2, false, false);
        app.show_output();
        match action {
            "problems" => {
                key(&mut app, KeyCode::Char('M'), ctrl_shift);
                assert!(app.show_panel && app.panel_tab == 1);
                assert_eq!(app.active_pane, ActivePane::Editor);
                key(&mut app, KeyCode::Char('M'), ctrl_shift);
                assert!(!app.show_panel);
            }
            "zen" => {
                key(&mut app, KeyCode::F(11), KeyModifiers::NONE);
                assert!(app.zen_mode && !app.show_panel);
            }
            "maximize" => {
                app.execute_palette_command("toggle_maximized");
                assert!(!app.show_panel);
            }
            "search" => {
                key(&mut app, KeyCode::Char('F'), ctrl_shift);
                assert!(app.global_search_active);
                assert_eq!(app.active_pane, ActivePane::Explorer);
                assert!(!app.logs.search_focused());
                continue;
            }
            "explorer" => {
                key(&mut app, KeyCode::Char('E'), ctrl_shift);
                assert_eq!(app.active_pane, ActivePane::Explorer);
                continue;
            }
            _ => unreachable!(),
        }
        assert_eq!(app.active_pane, ActivePane::Editor, "{action}");
        let version = app.editor().unwrap().content_version;
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(
            app.editor().unwrap().content_version > version,
            "first typing after {action} must reach the visible editor"
        );
    }
}

#[test]
fn switching_editor_tabs_from_output_focuses_the_chosen_document() {
    for code in [KeyCode::Tab, KeyCode::PageDown] {
        let mut app = fixture(2, false, false);
        app.tabs
            .push(Tab::new_file(Editor::test_with_text("second buffer")));
        app.show_output();
        key(&mut app, code, KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.active_pane, ActivePane::Editor);
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(app.tabs[1].editor.rope.to_string().contains('x'));
        assert!(!app.tabs[0].editor.rope.to_string().contains('x'));
    }
}

#[test]
fn maximizing_or_dragging_a_panel_in_one_desk_cannot_change_another_desks_panel() {
    let mut app = fixture(1, true, false);
    markers(&mut app, 80);
    draw(&mut app);
    let bar = app.panel_chrome.scrollbar.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        (bar.area.x, bar.area.y + bar.thumb_top),
    );
    let desk = app.new_desk("Other", false);
    assert_eq!(app.desks.active, desk);
    assert!(app.panel_area.is_none() && app.panel_chrome.scrollbar.is_none());
    app.show_output();
    draw(&mut app);
    let before = app.problem_scroll;
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        (0, u16::MAX),
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        (0, u16::MAX),
    );
    assert_eq!(app.problem_scroll, before);
    let maximize = app.panel_chrome.maximize;
    click(&mut app, (maximize.x, maximize.y));
    assert!(app.panel_chrome.maximized);
    app.new_desk("Next", false);
    app.show_output();
    draw(&mut app);
    assert!(!app.panel_chrome.maximized);
    assert!(app.panel_area.unwrap().height < 25);
}
