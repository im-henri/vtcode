use super::*;
use crate::app::{EditorGroup, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

fn populated_history() -> App {
    let mut app = app();
    for index in 0..55 {
        app.editor_mut().unwrap().navigation_id = Editor::allocate_navigation_id();
        app.editor_mut().unwrap().file_path = Some(format!("workspace/subdirectory/source/file_{index}.rs").into());
        app.editor_mut().unwrap().cursor_line = 50;
        app.finish_navigation();
    }
    assert_eq!(app.navigation.entries.len(), 50);
    app
}

#[test]
fn caret_history_fast_path_preserves_jumps_selections_forward_branch_and_path_changes() {
    let mut actual = populated_history();
    let mut expected = populated_history();
    expected.tabs[0].editor.navigation_id = actual.tabs[0].editor.navigation_id;
    expected.navigation.entries = actual.navigation.entries.clone();
    expected.navigation.reference_caret_recording = true;
    let path_storage = actual.navigation.entries.last().unwrap().path.as_ref().unwrap().as_os_str().as_encoded_bytes().as_ptr();
    for step in 0..200 {
        for app in [&mut actual, &mut expected] {
            if step >= 50 && step % 17 == 0 { app.push_nav_history(); }
            let editor = app.editor_mut().unwrap();
            editor.cursor_line = if step < 50 { 50 } else { (step * 7) % 150 };
            editor.cursor_col = step % 20;
            editor.selection_anchor = (step % 3 == 0).then_some((editor.cursor_line.saturating_sub(2), 1));
            if step == 80 { editor.file_path = Some("workspace/renamed.rs".into()); }
            if step == 90 { app.active_group += 1; }
            if step == 100 { app.navigation.index = Some(app.navigation.entries.len() - 3); }
            app.finish_navigation();
        }
        assert_eq!(actual.navigation.entries, expected.navigation.entries, "step={step}");
        assert_eq!(actual.navigation.index, expected.navigation.index, "step={step}");
        assert_eq!(actual.navigation.jump_pending, expected.navigation.jump_pending);
        assert_eq!(actual.navigation.can_navigate(false), expected.navigation.can_navigate(false));
        assert_eq!(actual.navigation.can_navigate(true), expected.navigation.can_navigate(true));
        if step < 17 {
            assert_eq!(actual.navigation.entries.last().unwrap().path.as_ref().unwrap().as_os_str().as_encoded_bytes().as_ptr(), path_storage);
        }
    }
}

#[test]
#[ignore = "manual optimized-profile keyboard history comparison"]
fn benchmark_low_end_keyboard_history() {
    use std::time::Instant;
    let mut times = [Vec::new(), Vec::new()];
    for round in 0..7 {
        for reference in [round % 2 == 0, round % 2 != 0] {
            let mut app = populated_history();
            app.navigation.reference_caret_recording = reference;
            let started = Instant::now();
            for _ in 0..1_000 {
                for code in [KeyCode::Down, KeyCode::Right, KeyCode::Up, KeyCode::Left] {
                    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code, KeyModifiers::NONE));
                }
            }
            times[usize::from(reference)].push(started.elapsed() / 4_000);
            assert_eq!(cursor(&app), (50, 0));
            assert_eq!(app.navigation.entries.len(), 50);
        }
    }
    for samples in &mut times { samples.sort(); }
    eprintln!("keyboard input with 50 history stops: before={:?}, after={:?}", times[1][3], times[0][3]);
}

fn tab() -> Tab {
    let mut editor = Editor::new();
    editor.rope = ropey::Rope::from_str(&"abcdefghijklmnopqrstuvwxyz\n".repeat(200));
    Tab::new_file(editor)
}

fn app() -> App {
    let mut app = App::test_new();
    app.tabs = vec![tab()];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.record_navigation();
    app
}

fn move_to(app: &mut App, line: usize, col: usize) {
    app.editor_mut().unwrap().set_cursor(line, col);
    app.finish_navigation();
}

fn cursor(app: &App) -> (usize, usize) {
    let editor = app.editor().unwrap();
    (editor.cursor_line, editor.cursor_col)
}

fn mouse(app: &mut App, kind: MouseEventKind) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn navigation_frame(
    app: &mut App,
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    app.poll_background_results();
    app.finish_navigation();
    app.sync_duplicate_file_tabs();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    app.finish_navigation();
}

#[test]
fn startup_explorer_clicks_can_be_retraced_without_entering_the_editor() {
    let workspace = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..3)
        .map(|i| workspace.path().join(format!("visit-{i}.txt")))
        .collect();
    for path in &paths {
        std::fs::write(path, "unchanged file\n").unwrap();
    }
    for preview in [false, true] {
        for double_click in [false, true] {
            for open_editors in [false, true] {
                let mut app = App::new_deferred(workspace.path().to_path_buf());
                app.enable_preview_tabs = preview;
                app.show_open_editors = open_editors;
                app.refresh_explorer_async();
                app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                    app.explorer.entries.len() == 3
                });
                let mut terminal =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(150, 30)).unwrap();
                for path in &paths {
                    for _ in 0..if double_click { 2 } else { 1 } {
                        navigation_frame(&mut app, &mut terminal);
                        let area = app.explorer_area.expect("Explorer is visible at startup");
                        let name = path.file_name().unwrap().to_str().unwrap();
                        let buffer = terminal.backend().buffer();
                        let (column, row) = (area.y..area.bottom())
                            .rev()
                            .find_map(|row| {
                                (area.x..area.right().saturating_sub(name.len() as u16))
                                    .find(|&column| {
                                        let text: String = (column..column + name.len() as u16)
                                            .map(|x| buffer.cell((x, row)).unwrap().symbol())
                                            .collect();
                                        text == name
                                    })
                                    .map(|column| (column, row))
                            })
                            .expect("file is visible in the rendered Explorer");
                        for kind in [
                            MouseEventKind::Down(MouseButton::Left),
                            MouseEventKind::Up(MouseButton::Left),
                        ] {
                            crate::mouse::handle_mouse(
                                &mut app,
                                MouseEvent {
                                    kind,
                                    column,
                                    row,
                                    modifiers: KeyModifiers::NONE,
                                },
                            );
                        }
                        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(path));
                    }
                    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                        !app.editor().unwrap().bg_loading
                    });
                    navigation_frame(&mut app, &mut terminal);
                }
                for (button, expected) in [
                    (MouseButton::Back, 1),
                    (MouseButton::Back, 0),
                    (MouseButton::Forward, 1),
                    (MouseButton::Forward, 2),
                ] {
                    mouse(&mut app, MouseEventKind::Down(button));
                    mouse(&mut app, MouseEventKind::Up(button));
                    navigation_frame(&mut app, &mut terminal);
                    assert_eq!(
                        app.editor().unwrap().file_path.as_ref(),
                        Some(&paths[expected]),
                        "preview={preview}, double_click={double_click}, open_editors={open_editors}, button={button:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn file_only_navigation_matches_a_visit_stack_across_open_close_and_replay() {
    let workspace = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..4)
        .map(|i| workspace.path().join(format!("visit-{i}.txt")))
        .collect();
    for path in &paths {
        std::fs::write(path, "unchanged file\n").unwrap();
    }
    for preview in [false, true] {
        for completion_interval in [1, 7] {
            for seed in 1..=8u64 {
                let mut app = App::test_new();
                app.enable_preview_tabs = preview;
                let mut visits: Vec<PathBuf> = Vec::new();
                let mut index: Option<usize> = None;
                let mut random = seed;
                let mut trace = Vec::new();
                // Mix lifecycle actions, then walk both ends of the complete
                // 50-entry history, including any files that must be reopened.
                for step in 0..228 {
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let action = if step < 128 {
                        ((random >> 32) % 10) as usize
                    } else if step < 178 {
                        8
                    } else {
                        9
                    };
                    trace.push(action);
                    if action < 4 {
                        app.open_file_preview(&paths[action]);
                    } else if action < 8 {
                        if let Some(tab) = app.tabs.iter().position(|tab| {
                            tab.editor.file_path.as_ref() == Some(&paths[action - 4])
                        }) {
                            app.close_tab(tab);
                        }
                    } else {
                        let forward = action == 9;
                        let mut target = index;
                        if let Some(current) = index {
                            while let Some(next) = if forward {
                                (target.unwrap() + 1 < visits.len()).then_some(target.unwrap() + 1)
                            } else {
                                target.unwrap().checked_sub(1)
                            } {
                                target = Some(next);
                                if visits[next] != visits[current] {
                                    break;
                                }
                            }
                        }
                        let expected = if target != index {
                            target.map(|target| visits[target].clone())
                        } else {
                            app.editor().and_then(|editor| editor.file_path.clone())
                        };
                        index = target;
                        mouse(
                            &mut app,
                            MouseEventKind::Down(if forward {
                                MouseButton::Forward
                            } else {
                                MouseButton::Back
                            }),
                        );
                        assert_eq!(
                            app.editor().and_then(|editor| editor.file_path.clone()),
                            expected,
                            "preview={preview}, completion={completion_interval}, seed={seed}, step={step}, actions={trace:?}"
                        );
                    }
                    if action < 8 {
                        if let Some(path) = app.editor().and_then(|editor| editor.file_path.clone())
                        {
                            if index.is_none_or(|index| visits[index] != path) {
                                visits.truncate(index.map_or(0, |index| index + 1));
                                visits.push(path);
                                if visits.len() > 50 {
                                    visits.remove(0);
                                }
                                index = Some(visits.len() - 1);
                            }
                        }
                    }
                    if step % completion_interval == 0 {
                        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                            app.tabs.iter().all(|tab| !tab.editor.bg_loading)
                        });
                    }
                    app.finish_navigation();
                    for tab in &app.tabs {
                        assert_eq!((tab.editor.cursor_line, tab.editor.cursor_col), (0, 0));
                        assert!(!tab.editor.modified);
                    }
                }
            }
        }
    }
}

#[test]
fn reopening_the_last_closed_file_does_not_insert_a_loading_history_stop() {
    let workspace = tempfile::tempdir().unwrap();
    let a = workspace.path().join("a.txt");
    let b = workspace.path().join("b.txt");
    for path in [&a, &b] {
        std::fs::write(path, "unchanged\n").unwrap();
    }
    let mut app = App::test_new();
    for path in [&a, &b] {
        app.open_file(path);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.editor().unwrap().bg_loading
        });
        app.finish_navigation();
    }
    app.close_tab(app.active_tab); // B closes, revealing A.
    app.close_tab(app.active_tab); // The workbench is empty.
    let count = app.navigation.entries.len();
    app.open_file(&a);
    assert!(app.editor().unwrap().bg_loading);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&b));
    assert_eq!(app.navigation.entries.len(), count);
}

#[test]
fn reopening_the_current_history_file_preserves_the_forward_branch() {
    let workspace = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..3)
        .map(|i| workspace.path().join(format!("{i}.txt")))
        .collect();
    let mut app = App::test_new();
    for path in &paths {
        std::fs::write(path, "unchanged\n").unwrap();
        app.open_file(path);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.editor().unwrap().bg_loading
        });
        app.finish_navigation();
    }
    app.navigate_back(); // B, with C still ahead.
    app.close_tab(2);
    app.close_tab(0);
    app.close_tab(0);
    app.open_file(&paths[1]);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[2]));
}

fn loaded_file_tabs(count: usize) -> (tempfile::TempDir, Vec<PathBuf>, App) {
    let workspace = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..count)
        .map(|i| workspace.path().join(format!("file-{i}.txt")))
        .collect();
    let mut app = App::test_new();
    for path in &paths {
        std::fs::write(path, "unchanged\n").unwrap();
        app.open_file(path);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.editor().unwrap().bg_loading
        });
        app.finish_navigation();
    }
    (workspace, paths, app)
}

#[test]
fn bulk_closes_keep_the_active_file_and_its_forward_history() {
    for command in ["close_left", "close_right", "close_others"] {
        for protected in [false, true] {
            let (_workspace, paths, mut app) = loaded_file_tabs(4);
            if protected {
                app.tabs[0].pinned = true;
                app.tabs[1].editor.modified = true;
            }
            app.navigate_back(); // C, with D still ahead.
            app.execute_palette_command(command);
            assert_eq!(
                app.editor().unwrap().file_path.as_ref(),
                Some(&paths[2]),
                "{command}"
            );
            let remaining: Vec<_> = app
                .tabs
                .iter()
                .map(|tab| tab.editor.file_path.clone().unwrap())
                .collect();
            let expected = match (command, protected) {
                ("close_left", true) => paths.clone(),
                ("close_left", false) => vec![paths[2].clone(), paths[3].clone()],
                ("close_right", _) | ("close_others", true) => paths[..3].to_vec(),
                _ => vec![paths[2].clone()],
            };
            assert_eq!(remaining, expected, "{command}");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
            assert_eq!(
                app.editor().unwrap().file_path.as_ref(),
                Some(&paths[3]),
                "{command}"
            );
            mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
            assert_eq!(
                app.editor().unwrap().file_path.as_ref(),
                Some(&paths[2]),
                "{command}"
            );
        }
    }
}

#[test]
fn close_all_in_a_single_pane_keeps_file_navigation_history() {
    let (_workspace, paths, mut app) = loaded_file_tabs(3);
    app.execute_palette_command("tab_close_all");
    assert!(app.tabs.is_empty());
    assert_eq!(app.navigation.entries.len(), 3);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(
        app.editor().and_then(|editor| editor.file_path.as_ref()),
        Some(&paths[1])
    );
    mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[2]));
}

fn click_rendered_tab(app: &mut App, tab: usize, close: bool, sidebar: bool) {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(150, 30)).unwrap();
    app.show_explorer = true;
    app.show_open_editors = true;
    navigation_frame(app, &mut terminal);
    let (column, row) = if sidebar {
        let area = app.explorer_area.unwrap();
        (
            if close { area.right() - 2 } else { area.x + 3 },
            area.y + 2 + tab as u16,
        )
    } else {
        let area = app.tab_bar_area.unwrap();
        let columns: Vec<_> = (area.x..area.right())
            .filter(|&column| crate::mouse::find_tab_at_col(app, column, area) == Some(tab))
            .collect();
        (
            if close {
                *columns.last().unwrap()
            } else {
                columns[1]
            },
            area.y,
        )
    };
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

#[test]
fn clicking_tabs_then_closing_one_returns_to_the_last_visited_file() {
    for sidebar in [false, true] {
        let (_workspace, paths, mut app) = loaded_file_tabs(3);
        click_rendered_tab(&mut app, 0, false, sidebar);
        click_rendered_tab(&mut app, 1, false, sidebar);
        click_rendered_tab(&mut app, 1, true, sidebar);
        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[0]));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[1]));
    }
}

#[test]
fn closing_a_split_keeps_the_surviving_panes_active_file() {
    let (_workspace, paths, mut app) = loaded_file_tabs(4);
    app.create_split_group(3);
    app.finish_navigation();
    app.close_tab(0);
    assert!(app.groups.is_empty());
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[2]));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[1]));
}

#[test]
fn vscode_selection_history_merges_nearby_moves_and_round_trips_ranges() {
    let mut app = app();
    move_to(&mut app, 0, 2);
    move_to(&mut app, 14, 1);
    move_to(&mut app, 15, 1);
    move_to(&mut app, 16, 1);
    app.editor_mut().unwrap().selection_anchor = Some((29, 5));
    move_to(&mut app, 29, 8);
    move_to(&mut app, 39, 1);
    // The selection start, rather than the moving end, defines a stop.
    assert_eq!(app.navigation.entries.len(), 3);
    app.editor_mut().unwrap().clear_selection();
    app.finish_navigation();
    assert_eq!(app.navigation.entries.len(), 4);
    app.navigate_back();
    assert_eq!(cursor(&app), (39, 1));
    assert_eq!(app.editor().unwrap().selection_anchor, Some((29, 5)));
    app.navigate_back();
    assert_eq!(cursor(&app), (16, 1));
    app.navigate_back();
    assert_eq!(cursor(&app), (0, 2));
    app.navigate_forward();
    assert_eq!(cursor(&app), (16, 1));
}

#[test]
fn explicit_nearby_jumps_are_separate_but_same_line_moves_are_not() {
    let mut app = app();
    move_to(&mut app, 1, 2);
    app.push_nav_history();
    move_to(&mut app, 4, 3);
    app.push_nav_history();
    move_to(&mut app, 5, 7);
    app.push_nav_history();
    move_to(&mut app, 5, 12);
    app.navigate_back();
    assert_eq!(cursor(&app), (4, 3));
    app.navigate_back();
    assert_eq!(cursor(&app), (1, 2));
    app.navigate_forward();
    app.navigate_forward();
    assert_eq!(cursor(&app), (5, 12));
}

#[test]
fn back_then_small_move_keeps_forward_but_new_jump_discards_it() {
    let mut app = app();
    move_to(&mut app, 30, 3);
    move_to(&mut app, 60, 6);
    app.navigate_back();
    move_to(&mut app, 31, 4);
    app.navigate_forward();
    assert_eq!(cursor(&app), (60, 6));
    app.navigate_back();
    app.push_nav_history();
    move_to(&mut app, 34, 5);
    app.navigate_forward();
    assert_eq!(cursor(&app), (34, 5));
    app.navigate_back();
    assert_eq!(cursor(&app), (31, 4));
}

#[test]
fn history_has_fifty_stops_and_boundaries_are_noops() {
    let mut app = app();
    for index in 1..80 {
        app.push_nav_history();
        move_to(&mut app, index, 0);
    }
    assert_eq!(app.navigation.entries.len(), 50);
    for _ in 0..100 {
        app.navigate_back();
    }
    assert_eq!(cursor(&app), (30, 0));
    assert!(!app.navigation.can_navigate(false));
    assert!(app.navigation.can_navigate(true));
    for _ in 0..100 {
        app.navigate_forward();
    }
    assert_eq!(cursor(&app), (79, 0));
    assert!(app.navigation.can_navigate(false));
    assert!(!app.navigation.can_navigate(true));
    assert_eq!(app.navigation.entries.len(), 50);
    let mut empty = App::test_new();
    empty.navigate_back();
    empty.navigate_forward();
    assert!(empty.navigation.entries.is_empty());
    assert!(!empty.navigation.can_navigate(false));
    assert!(!empty.navigation.can_navigate(true));
}

#[test]
fn side_buttons_work_anywhere_and_only_on_press() {
    let mut app = app();
    move_to(&mut app, 30, 3);
    move_to(&mut app, 60, 6);
    app.active_pane = ActivePane::Explorer;
    app.context_menu_active = true;
    app.terminal_panel_focused = true;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(cursor(&app), (30, 3));
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert!(!app.context_menu_active);
    assert!(!app.terminal_panel_focused);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Back));
    mouse(&mut app, MouseEventKind::Up(MouseButton::Back));
    assert_eq!(cursor(&app), (30, 3));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
    mouse(&mut app, MouseEventKind::Up(MouseButton::Forward));
    assert_eq!(cursor(&app), (60, 6));
    app.mouse_back_forward_to_navigate = false;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(cursor(&app), (60, 6));
    app.execute_palette_command("navigate_back");
    assert_eq!(cursor(&app), (30, 3));
}

#[test]
fn keyboard_cursor_moves_feed_the_same_history_as_mouse_and_commands() {
    let mut app = app();
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
    );
    let destination = cursor(&app);
    assert!(destination.0 >= 10);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(cursor(&app), (0, 0));
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
    );
    assert_eq!(cursor(&app), destination);
}

#[test]
fn short_bracket_and_bookmark_keyboard_jumps_can_be_retraced() {
    for key in [
        KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::ALT),
        KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::F(2), KeyModifiers::SHIFT),
    ] {
        let mut app = app();
        let editor = app.editor_mut().unwrap();
        editor.rope = ropey::Rope::from_str("{\n  content\n}\n");
        editor.bookmarks.insert(2);
        crate::input_keyboard::handle_keyboard_event(&mut app, key);
        assert_eq!(cursor(&app), (2, 0), "{key:?}");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
        assert_eq!(cursor(&app), (0, 0), "{key:?}");
        mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
        assert_eq!(cursor(&app), (2, 0), "{key:?}");
    }
}

#[test]
fn tab_reordering_and_closing_an_unrelated_tab_preserve_identity() {
    let mut app = app();
    let first = app.editor().unwrap().navigation_id;
    app.tabs.push(tab());
    app.tabs.push(tab());
    app.active_tab = 2;
    move_to(&mut app, 45, 9);
    let last = app.editor().unwrap().navigation_id;
    app.tabs.swap(0, 1);
    app.force_close_tab(0);
    app.navigate_back();
    assert_eq!(app.editor().unwrap().navigation_id, first);
    app.navigate_forward();
    assert_eq!(app.editor().unwrap().navigation_id, last);
    assert_eq!(cursor(&app), (45, 9));
}

#[test]
fn closed_untitled_buffers_are_removed_instead_of_targeting_reused_indexes() {
    let mut app = app();
    app.tabs.push(tab());
    app.active_tab = 1;
    move_to(&mut app, 40, 0);
    app.force_close_tab(0);
    let id = app.editor().unwrap().navigation_id;
    app.navigate_back();
    assert_eq!(app.editor().unwrap().navigation_id, id);
    assert_eq!(cursor(&app), (40, 0));
}

#[test]
fn split_navigation_tracks_view_moves_and_group_removal() {
    let mut app = app();
    let first = app.editor().unwrap().navigation_id;
    app.tabs.push(tab());
    app.active_tab = 1;
    move_to(&mut app, 40, 4);
    let second = app.editor().unwrap().navigation_id;
    app.create_split_group(1);
    app.finish_navigation();
    app.navigate_back();
    assert_eq!(app.editor().unwrap().navigation_id, first);
    app.navigate_forward();
    assert_eq!(app.editor().unwrap().navigation_id, second);
    assert_eq!(cursor(&app), (40, 4));
    let target = 1 - app.active_group;
    assert!(app.move_tab_between_groups(app.active_group, app.active_tab, target));
    app.finish_navigation();
    app.navigate_back();
    app.navigate_forward();
    assert!(app.navigation.entries.iter().all(|entry| entry.group == 0));
    assert!(app.groups.is_empty());
}

#[test]
fn same_file_in_two_groups_restores_the_original_view_and_selection() {
    let mut app = app();
    app.tabs[0].editor.file_path = Some(PathBuf::from("same-file.txt"));
    app.navigation = NavigationHistory::default();
    move_to(&mut app, 5, 2);
    let mut other = EditorGroup::new();
    other.tabs.push(tab());
    other.tabs[0].editor.file_path = Some(PathBuf::from("same-file.txt"));
    other.tabs[0].editor.set_cursor(70, 4);
    app.groups = vec![EditorGroup::new(), other];
    app.switch_group(1);
    app.finish_navigation();
    app.navigate_back();
    assert_eq!(app.active_group, 0);
    assert_eq!(cursor(&app), (5, 2));
    app.navigate_forward();
    assert_eq!(app.active_group, 1);
    assert_eq!(cursor(&app), (70, 4));
}

#[test]
fn replaced_preview_and_closed_file_reopen_with_deferred_selection() {
    let workspace = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..3)
        .map(|i| workspace.path().join(format!("{i}.txt")))
        .collect();
    for path in &paths {
        std::fs::write(path, "abcdefghijklmnopqrstuvwxyz\n".repeat(100)).unwrap();
    }
    let mut app = App::test_new();
    app.open_file_preview(&paths[0]);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    app.editor_mut().unwrap().selection_anchor = Some((33, 8));
    move_to(&mut app, 31, 2);
    app.open_file_preview(&paths[1]);
    app.open_file_preview(&paths[2]);
    app.navigate_back();
    app.navigate_back();
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[0]));
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    assert_eq!(cursor(&app), (31, 2));
    assert_eq!(app.editor().unwrap().selection_anchor, Some((33, 8)));
    app.navigate_forward();
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[1]));
    app.navigate_forward();
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&paths[2]));
}

#[test]
fn goto_line_preview_is_transient_and_accept_records_a_nearby_jump() {
    let mut app = app();
    move_to(&mut app, 5, 3);
    app.execute_palette_command("goto_line");
    app.goto_line_query = "9:5".into();
    app.preview_goto_line();
    app.finish_navigation();
    assert_eq!(app.navigation.entries.len(), 1);
    app.goto_line();
    assert_eq!(cursor(&app), (8, 4));
    app.navigate_back();
    assert_eq!(cursor(&app), (5, 3));
    app.navigate_forward();
    assert_eq!(cursor(&app), (8, 4));
}

#[test]
fn navigation_reveals_folded_targets_and_clamps_positions_after_edits() {
    let mut app = app();
    move_to(&mut app, 30, 23);
    move_to(&mut app, 90, 3);
    app.editor_mut().unwrap().folded_ranges.insert(20, 50);
    app.navigate_back();
    assert_eq!(cursor(&app), (30, 23));
    assert!(app.editor().unwrap().folded_ranges.is_empty());
    app.editor_mut().unwrap().rope = ropey::Rope::from_str("short\ntext");
    app.navigate_forward();
    assert_eq!(cursor(&app), (1, 3));
}

#[test]
fn unavailable_closed_files_are_skipped_in_both_directions() {
    for forward in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..3)
            .map(|i| workspace.path().join(format!("{i}.txt")))
            .collect();
        for path in &paths {
            std::fs::write(path, "file contents").unwrap();
        }
        let mut app = App::test_new();
        for path in &paths {
            app.open_file(path);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                !app.editor().unwrap().bg_loading
            });
            app.finish_navigation();
        }
        if forward {
            app.navigate_back();
            app.navigate_back();
        }
        app.force_close_tab(1);
        std::fs::remove_file(&paths[1]).unwrap();
        if forward {
            app.navigate_forward();
        } else {
            app.navigate_back();
        }
        let expected = &paths[if forward { 2 } else { 0 }];
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            app.editor().is_some_and(|editor| {
                editor.file_path.as_ref() == Some(expected) && !editor.bg_loading
            })
        });
        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(expected));
        assert!(
            !app.navigation
                .entries
                .iter()
                .any(|entry| entry.path.as_ref() == Some(&paths[1]))
        );
        assert!(
            !app.tabs
                .iter()
                .any(|tab| tab.editor.file_path.as_ref() == Some(&paths[1]))
        );
    }
}

#[test]
fn renamed_closed_file_is_reopened_at_the_new_path() {
    let workspace = tempfile::tempdir().unwrap();
    let old = workspace.path().join("old.txt");
    let new = workspace.path().join("new.txt");
    std::fs::write(&old, "line\n".repeat(80)).unwrap();
    let mut app = App::test_new();
    app.open_file(&old);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    move_to(&mut app, 31, 2);
    app.new_file();
    app.force_close_tab(0);
    app.rename.target = Some(old.clone());
    app.rename.query = "new.txt".into();
    app.execute_rename();
    app.navigate_back();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&new));
    assert_eq!(cursor(&app), (31, 2));
}

#[test]
fn mouse_navigation_setting_round_trips_and_keyboard_navigation_is_global() {
    let mut app = app();
    move_to(&mut app, 40, 2);
    let settings: crate::settings::Settings =
        serde_json::from_str(r#"{"workbench.editor.mouseBackForwardToNavigate":false}"#).unwrap();
    app.apply_settings(&settings);
    assert!(!app.mouse_back_forward_to_navigate);
    assert_eq!(
        serde_json::to_value(app.current_settings()).unwrap()["workbench.editor.mouseBackForwardToNavigate"],
        false
    );
    app.active_pane = ActivePane::Explorer;
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
    );
    assert_eq!(cursor(&app), (0, 0));
    app.terminal_panel_focused = true;
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::ALT),
    );
    assert_eq!(cursor(&app), (40, 2));
}

#[test]
fn wheel_scrolling_and_cancelled_goto_previews_do_not_make_history_stops() {
    let mut app = app();
    move_to(&mut app, 20, 2);
    let count = app.navigation.entries.len();
    app.editor_mut().unwrap().scroll_offset = 100;
    app.finish_navigation();
    app.open_goto_line();
    app.goto_line_query = "140".into();
    app.preview_goto_line();
    app.finish_navigation();
    app.cancel_goto_line();
    app.finish_navigation();
    assert_eq!(app.navigation.entries.len(), count);
    assert_eq!(cursor(&app), (20, 2));
}

#[test]
fn clamping_an_asynchronously_reopened_file_preserves_forward_history() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("shrinks.txt");
    std::fs::write(&path, "long line\n".repeat(100)).unwrap();
    let mut app = App::test_new();
    app.open_file(&path);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    move_to(&mut app, 70, 7);
    app.new_file();
    let destination = app.editor().unwrap().navigation_id;
    app.force_close_tab(0);
    std::fs::write(&path, "tiny").unwrap();
    app.navigate_back();
    let count = app.navigation.entries.len();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    app.finish_navigation();
    assert_eq!(cursor(&app), (0, 4));
    assert_eq!(app.navigation.entries.len(), count);
    app.navigate_forward();
    assert_eq!(app.editor().unwrap().navigation_id, destination);
}

#[test]
fn a_new_jump_while_loading_supersedes_the_pending_history_selection() {
    let mut editor = Editor::new_loading(Path::new("still-loading.txt"), 0);
    NavigationSelection {
        cursor: (50, 5),
        anchor: Some((50, 2)),
    }
    .restore(&mut editor);
    editor.set_cursor(80, 4);
    assert_eq!(editor.pending_cursor_position, Some((80, 4)));
    assert_eq!(editor.pending_navigation_selection, None);
}

#[test]
fn closing_an_intervening_view_does_not_leave_a_noop_history_step() {
    let mut app = app();
    move_to(&mut app, 30, 3);
    app.new_file();
    app.active_tab = 0;
    app.finish_navigation();
    app.force_close_tab(1);
    assert!(app.navigation.can_navigate(false));
    assert!(!app.navigation.can_navigate(true));
    app.navigate_back();
    assert_eq!(cursor(&app), (0, 0));
    assert!(!app.navigation.can_navigate(false));
    assert!(app.navigation.can_navigate(true));
    app.navigate_forward();
    assert_eq!(cursor(&app), (30, 3));
    assert!(!app.navigation.can_navigate(true), "identical remaining stops are disabled");
}

#[test]
fn back_skips_positions_that_collapse_after_the_document_shrinks() {
    let mut app = app();
    let path = PathBuf::from("shrinking-history.txt");
    app.editor_mut().unwrap().file_path = Some(path.clone());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    navigation_frame(&mut app, &mut terminal);
    move_to(&mut app, 30, 0);
    move_to(&mut app, 60, 0);
    // An external edit removes both visited locations. The reload clamps the
    // current caret, but the older history entries still refer to removed lines.
    let rope = ropey::Rope::from_str(&"text\n".repeat(8));
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileReloaded {
            targets: app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs))
                .map(|tab| (tab.editor.navigation_id, tab.editor.content_version)).collect(),
            workspace_generation: 0,
            path,
            result: Ok(crate::editor::LoadedTextFile {
            remote_checkpoint: None,
                file_size: rope.len_bytes() as u64,
                rope,
                encoding: "UTF-8",
                last_disk_modified: None,
            }),
        })
        .unwrap();
    navigation_frame(&mut app, &mut terminal);
    assert_eq!(cursor(&app), (8, 0));

    let count = app.navigation.entries.len();
    for _ in 0..3 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
        navigation_frame(&mut app, &mut terminal);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Back));
        assert_eq!(cursor(&app), (0, 0));
        mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
        navigation_frame(&mut app, &mut terminal);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Forward));
        assert_eq!(cursor(&app), (8, 0));
        assert_eq!(app.navigation.entries.len(), count);
    }
}

#[test]
fn forward_skips_saved_positions_that_now_resolve_to_the_current_caret() {
    let mut app = app();
    move_to(&mut app, 30, 0);
    move_to(&mut app, 60, 0);
    app.new_file();
    let destination = app.editor().unwrap().navigation_id;
    app.navigate_back();
    app.navigate_back();
    app.navigate_back();
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::new();
    editor.clamp_cursor();
    app.finish_navigation();

    mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
    assert_eq!(app.editor().unwrap().navigation_id, destination);
}

#[test]
fn a_selection_that_was_deleted_does_not_leave_an_invisible_back_stop() {
    let mut app = app();
    app.editor_mut().unwrap().selection_anchor = Some((30, 2));
    move_to(&mut app, 35, 4);
    app.editor_mut().unwrap().selection_anchor = Some((60, 2));
    move_to(&mut app, 65, 4);
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str("text\n");
    editor.clear_selection();
    editor.clamp_cursor();
    app.finish_navigation();

    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(cursor(&app), (0, 0));
}

#[test]
fn back_skips_a_loading_visit_that_resolves_to_the_current_position() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("loading-visit.txt");
    std::fs::write(&path, "text\n".repeat(100)).unwrap();
    let mut app = app();
    let source = app.editor().unwrap().navigation_id;
    app.open_file(&path); // First visit has no known cursor yet.
    app.new_file();
    let temporary = app.active_tab;
    app.open_file(&path);
    move_to(&mut app, 60, 2);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    app.force_close_tab(temporary);

    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(app.editor().unwrap().navigation_id, source);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Forward));
    assert_eq!(cursor(&app), (60, 2));
}

#[test]
fn interrupted_reopen_does_not_overwrite_the_next_visited_editor() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("delayed.txt");
    std::fs::write(&path, "text\n".repeat(80)).unwrap();
    let mut app = App::test_new();
    app.open_file(&path);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    move_to(&mut app, 50, 2);
    app.new_file();
    app.force_close_tab(0);
    app.navigate_back();
    assert!(app.editor().unwrap().bg_loading);
    app.new_file(); // The user leaves the history target before it finishes loading.
    let visited = app.editor().unwrap().navigation_id;
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.tabs
            .iter()
            .any(|tab| tab.editor.file_path.as_ref() == Some(&path) && !tab.editor.bg_loading)
    });
    app.open_file(&path); // Revisiting it must add a stop, not complete the abandoned Back.
    app.navigate_back();
    assert_eq!(app.editor().unwrap().navigation_id, visited);
}

#[test]
fn closing_a_tab_before_the_active_tab_preserves_focus_and_forward_history() {
    let mut app = app();
    app.tabs.extend([tab(), tab()]);
    app.active_tab = 1;
    move_to(&mut app, 30, 3);
    let middle = app.editor().unwrap().navigation_id;
    app.active_tab = 2;
    move_to(&mut app, 60, 6);
    let last = app.editor().unwrap().navigation_id;
    app.navigate_back();
    app.force_close_tab(0);
    assert_eq!(app.editor().unwrap().navigation_id, middle);
    app.navigate_forward();
    assert_eq!(app.editor().unwrap().navigation_id, last);
    assert_eq!(cursor(&app), (60, 6));
}

#[test]
fn nearby_reference_jump_from_a_selection_has_its_own_history_stop() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("references.txt");
    let mut app = app();
    app.editor_mut().unwrap().file_path = Some(path.clone());
    move_to(&mut app, 20, 6);
    app.editor_mut().unwrap().selection_anchor = Some((20, 2));
    app.finish_navigation();
    app.lsp_references_active = true;
    app.lsp_manager.reference_locations = vec![lsp_types::Location {
        uri: lsp_types::Url::from_file_path(&path).unwrap(),
        range: lsp_types::Range::new(
            lsp_types::Position::new(24, 1),
            lsp_types::Position::new(24, 4),
        ),
    }];
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert_eq!(cursor(&app), (24, 1));
    mouse(&mut app, MouseEventKind::Down(MouseButton::Back));
    assert_eq!(cursor(&app), (20, 6));
    assert_eq!(app.editor().unwrap().selection_anchor, Some((20, 2)));
}

#[test]
fn explicit_commands_do_not_reuse_the_source_selection_anchor() {
    for command in [
        "next_bookmark",
        "prev_bookmark",
        "goto_last_edit",
        "goto_bracket",
    ] {
        let mut app = app();
        app.editor_mut().unwrap().rope = ropey::Rope::from_str(
            &(0..40)
                .map(|line| match line {
                    20 => "(\n",
                    24 => ")\n",
                    _ => "text\n",
                })
                .collect::<String>(),
        );
        move_to(&mut app, 20, 0);
        app.editor_mut().unwrap().selection_anchor = Some((20, 1));
        app.editor_mut().unwrap().bookmarks.insert(24);
        app.last_edit_position = Some((0, 24, 0));
        app.finish_navigation();
        app.execute_palette_command(command);
        assert_eq!(cursor(&app), (24, 0), "{command}");
        assert_eq!(app.editor().unwrap().selection_anchor, None, "{command}");
        app.navigate_back();
        assert_eq!(cursor(&app), (20, 0), "{command}");
        assert_eq!(
            app.editor().unwrap().selection_anchor,
            Some((20, 1)),
            "{command}"
        );
    }
}

#[test]
fn file_document_link_preserves_its_line_through_async_loading_and_history() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("linked.txt");
    std::fs::write(&path, "text\n".repeat(80)).unwrap();
    let mut app = app();
    app.code_area = Some(ratatui::layout::Rect::new(0, 0, 80, 10));
    let mut uri = lsp_types::Url::from_file_path(&path).unwrap();
    uri.set_fragment(Some("L50"));
    app.lsp_document_links = vec![(0, 0, 12, uri.to_string())];
    crate::mouse_selection::handle_editor_area_left_click(&mut app, 1, 0, KeyModifiers::CONTROL);
    app.finish_navigation();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    assert_eq!(cursor(&app), (49, 0));
    app.navigate_back();
    assert_eq!(app.editor().unwrap().file_path, None);
    app.navigate_forward();
    assert_eq!(cursor(&app), (49, 0));
}

#[test]
fn rapid_back_forward_survives_interleaved_file_completions() {
    let workspace = tempfile::tempdir().unwrap();
    let paths: Vec<_> = (0..3)
        .map(|i| workspace.path().join(format!("{i}.txt")))
        .collect();
    for path in &paths {
        std::fs::write(path, "text\n".repeat(100)).unwrap();
    }
    for schedule in 0u16..16 {
        let mut app = App::test_new();
        for (i, path) in paths.iter().enumerate() {
            app.open_file_preview(path);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                !app.editor().unwrap().bg_loading
            });
            move_to(&mut app, (i + 1) * 20, 2);
        }
        let stops = app.navigation.entries.clone();
        let mut index = stops.len() - 1;
        for (step, forward) in [false, false, false, true, false, true, true, true]
            .into_iter()
            .enumerate()
        {
            if forward {
                index += 1;
                app.navigate_forward();
            } else {
                index -= 1;
                app.navigate_back();
            }
            if schedule & (1 << (step % 4)) != 0 {
                app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                    !app.editor().unwrap().bg_loading
                });
            }
            app.finish_navigation();
            let actual = app.navigation_location().unwrap();
            assert_eq!(
                actual.path, stops[index].path,
                "schedule {schedule}, step {step}"
            );
            assert_eq!(
                actual.selection, stops[index].selection,
                "schedule {schedule}, step {step}"
            );
            assert_eq!(
                app.navigation.entries.len(),
                stops.len(),
                "schedule {schedule}, step {step}"
            );
        }
    }
}

#[test]
fn goto_during_a_reopen_preserves_history_before_and_after_loading() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("goto-loading.txt");
    std::fs::write(&path, "text\n".repeat(100)).unwrap();
    for finish_loading in [false, true] {
        for accept in [false, true] {
            let mut app = App::test_new();
            app.open_file(&path);
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                !app.editor().unwrap().bg_loading
            });
            app.editor_mut().unwrap().selection_anchor = Some((51, 1));
            move_to(&mut app, 50, 2);
            app.new_file();
            let forward = app.editor().unwrap().navigation_id;
            app.force_close_tab(0);
            app.navigate_back();
            app.execute_palette_command("goto_line");
            app.goto_line_query = "90".into();
            app.preview_goto_line();
            if finish_loading {
                app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                    !app.editor().unwrap().bg_loading
                });
                app.finish_navigation();
            }
            if accept {
                app.goto_line();
            } else {
                app.cancel_goto_line();
            }
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                !app.editor().unwrap().bg_loading
            });
            app.finish_navigation();
            if accept {
                assert_eq!(cursor(&app), (89, 0));
                app.navigate_back();
            }
            assert_eq!(cursor(&app), (50, 2));
            assert_eq!(app.editor().unwrap().selection_anchor, Some((51, 1)));
            app.navigate_forward();
            if accept {
                assert_eq!(cursor(&app), (89, 0));
            } else {
                assert_eq!(app.editor().unwrap().navigation_id, forward);
            }
        }
    }
}
