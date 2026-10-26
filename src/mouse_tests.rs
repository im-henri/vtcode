use super::*;
use crate::app::{App, Tab, TabKind};
use crate::editor::Editor;
use crate::mouse_overlay::handle_context_menu_mouse;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use std::path::PathBuf;

fn make_tab(name: &str) -> Tab {
    let mut editor = Editor::new();
    editor.file_path = Some(PathBuf::from(name));
    Tab::new_file(editor)
}

#[test]
fn terminal_layout_menu_items_are_disabled_without_an_open_terminal() {
    use ratatui::{backend::TestBackend, Terminal};

    for closed in [false, true] {
        for keyboard in [false, true] {
            for command in ["move_terminal_to_right", "detach_terminal_to_tab"] {
                let mut app = App::test_new();
                app.open_settings_tab();
                if closed {
                    app.toggle_terminal_sidebar();
                    app.close_terminal_tab();
                }
                app.menu_selected = 6;
                app.menu_active = true;
                app.menu_open = true;
                let items = app.menu_items(app.menu_selected);
                let index = items.iter().position(|(_, id)| *id == command).unwrap();
                let label = &items[index].0;
                app.menu_item_selected = index;
                let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
                terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                let buffer = terminal.backend().buffer();
                let (column, row) = (1..12).find_map(|row| {
                    (0..140 - label.len() as u16).find_map(|column| {
                        label.chars().enumerate().all(|(i, ch)| {
                            buffer[(column + i as u16, row)].symbol() == ch.to_string()
                        }).then_some((column, row))
                    })
                }).expect("terminal menu action is rendered");
                // Disabled actions stay dim even when selected or hovered.
                let enabled_row = row - index as u16;
                assert_ne!(buffer[(column, row)].fg, buffer[(column, enabled_row)].fg);
                let tabs = app.tabs.len();
                if keyboard {
                    crate::input_keyboard::handle_keyboard_event(&mut app,
                        event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                } else {
                    handle_mouse(&mut app, MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left), column, row,
                        modifiers: KeyModifiers::NONE,
                    });
                }
                assert!(app.menu_active && app.menu_open, "{command}");
                assert!(!app.show_terminal_sidebar && !app.show_panel);
                assert!(!app.terminal_panel_focused);
                assert_eq!(app.terminals.len(), 1);
                assert_eq!(app.tabs.len(), tabs);
                assert_eq!(app.tabs[app.active_tab].kind, TabKind::Settings);
            }
        }
    }
}

#[test]
fn new_terminal_menu_click_takes_priority_over_settings_search() {
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
    use crate::text_input::TextInputId;

    fn label_at(buffer: &Buffer, label: &str, mut rows: std::ops::Range<u16>) -> (u16, u16) {
        rows.find_map(|row| {
            (0..=buffer.area.width.saturating_sub(label.len() as u16)).find_map(|col| {
                label.chars().enumerate().all(|(i, ch)| {
                    buffer[(col + i as u16, row)].symbol() == ch.to_string()
                }).then_some((col, row))
            })
        }).expect("visible menu label")
    }

    fn click(app: &mut App, (column, row): (u16, u16)) {
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            handle_mouse(app, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
        }
    }

    for width in [140, 180] {
        for explorer in [false, true] {
            let mut app = App::test_new();
            app.show_explorer = explorer;
            app.open_settings_tab();
            app.settings_editor_search = "theme".into();
            app.settings_editor_search_cursor = 1;
            app.settings_editor_search_selection_anchor = Some(4);
            let mut terminal = Terminal::new(TestBackend::new(width, 35)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            let menu = label_at(terminal.backend().buffer(), "Terminal", 0..1);
            click(&mut app, menu);
            assert!(app.menu_active && app.menu_open);
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            let action = label_at(terminal.backend().buffer(), "New Terminal", 1..10);
            assert!(app.text_input_area(TextInputId::SettingsSearch).unwrap().contains(action.into()),
                "exercise a menu item directly above the Settings search field");
            click(&mut app, action);
            assert_eq!(app.terminals.len(), 1, "opening the first terminal reuses the empty slot");
            assert!(!app.menu_active && !app.menu_open);
            assert!(app.show_terminal_sidebar && app.terminal_sidebar_revealed);
            assert!(app.docked_terminal_has_focus());
            assert_eq!(app.tabs[app.active_tab].kind, TabKind::Settings);
            assert_eq!(app.settings_editor_search_cursor, 1);
            assert_eq!(app.settings_editor_search_selection_anchor, Some(4));
            assert!(app.text_input_drag.is_none());

            // Verify the resulting dock and keyboard routing without launching
            // the user's shell or executing any command on the host.
            let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "");
            let input = pty.test_capture_input();
            app.terminals[app.active_terminal].terminal_pty = Some(pty);
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            assert!(app.terminal_sidebar_area.is_some_and(|area| area.width > 3));
            crate::input_keyboard::handle_keyboard_event(&mut app,
                event::KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
            assert_eq!(&*input.lock().unwrap(), b"x");
            assert_eq!(app.settings_editor_search, "theme");
        }
    }
}

#[test]
fn edge_case_source_control_menu_uses_rendered_geometry() {
    use ratatui::{backend::TestBackend, Terminal};

    for (width, height, x, y) in [(80, 40, 4, 3), (80, 40, 75, 36), (40, 18, 36, 14), (80, 40, u16::MAX, u16::MAX)] {
        for action in ["Discard Selected", "Cancel"] {
            let mut app = App::test_new();
            app.sc_context_targets = ["first.rs", "second.rs"].into_iter().map(|path| crate::app::GitFileChange {
                path: path.into(), original_path: None, status: 'M', staged: false,
            }).collect();
            app.sc_context_target = Some(app.sc_context_targets[0].clone());
            app.sc_context_pos = (x, y);
            app.sc_context_active = true;
            let action_index = app.source_control_context_menu_items().iter().position(|item| *item == action).unwrap();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| crate::ui_dialogs::draw_source_control_context_menu(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let (col, row) = (0..height).find_map(|row| {
                (0..width.saturating_sub(action.len() as u16)).find_map(|col| {
                    let text: String = (col..col + action.len() as u16).map(|x| buffer[(x, row)].symbol()).collect();
                    // Include the far edge of long labels in hit testing.
                    (text == action).then_some((col + action.len() as u16 - 1, row))
                })
            }).expect("action is visible");
            app.sc_context_selected = usize::MAX;
            handle_mouse(&mut app, MouseEvent { kind: MouseEventKind::Moved, column: col, row, modifiers: KeyModifiers::NONE });
            assert_eq!(app.sc_context_selected, action_index, "{action} at {width}x{height}, anchor {x},{y}");
            handle_mouse(&mut app, MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE });
            assert!(!app.sc_context_active);
            assert_eq!(app.sc_discard_confirm_active, action == "Discard Selected");
            assert!(!app.bg_git_action_running, "no destructive action before confirmation");
        }
    }
}

#[test]
fn tab_compare_actions_work_with_mouse_and_keyboard_on_unsaved_files() {
    use ratatui::{backend::TestBackend, Terminal};

    fn choose(app: &mut App, tab: usize, command: &str, keyboard: bool) {
        app.tab_context_tab = tab;
        app.tab_context_pos = (0, 0);
        app.tab_context_active = true;
        let items = App::tab_context_items();
        let index = items.iter().position(|item| item.1 == command).unwrap();
        if keyboard {
            app.tab_context_selected = index;
            crate::input_dialogs::handle_tab_context_key(
                app,
                event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        } else {
            // Mouse hit testing uses this frame's bounds, independently of
            // the host console size (including headless test runs).
            let (width, height) = (80, 40);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| crate::ui_chrome::draw_tab_context_menu(frame, app)).unwrap();
            let label = items[index].0;
            let label_width = label.len() as u16;
            let buffer = terminal.backend().buffer();
            let (column, row) = (0..height).find_map(|row| {
                (0..width.saturating_sub(label_width)).find_map(|column| {
                    let text: String = (column..column + label_width)
                        .map(|x| buffer.cell((x, row)).unwrap().symbol())
                        .collect();
                    (text == label).then_some((column, row))
                })
            }).expect("rendered compare action");
            handle_mouse(app, MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            });
        }
        assert!(!app.tab_context_active);
    }

    for keyboard in [false, true] {
        let workspace = tempfile::tempdir().unwrap();
        let base = workspace.path().join("left.txt");
        let target = workspace.path().join("right.txt");
        std::fs::write(&base, "saved left\n").unwrap();
        std::fs::write(&target, "saved right\n").unwrap();
        let mut app = App::test_new();
        app.explorer.root = workspace.path().to_path_buf();
        app.tabs = vec![make_tab(base.to_str().unwrap()), make_tab(target.to_str().unwrap())];
        for (tab, content) in app.tabs.iter_mut().zip(["unsaved left\n", "unsaved right\n"]) {
            tab.editor.rope = ropey::Rope::from_str(content);
            tab.editor.modified = true;
            tab.editor.content_version = 1;
        }
        app.active_tab = 1;

        choose(&mut app, 0, "tab_select_compare", keyboard);
        assert_eq!(app.active_tab, 1, "selecting an inactive tab must keep focus");
        assert_eq!(app.selected_compare_path.as_ref(), Some(&base));
        assert_eq!(app.compare_base_path, None);
        choose(&mut app, 1, "tab_compare_selected", keyboard);
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            app.compare_diff_target_version.is_some() && !app.bg_diff_refresh_running
        });
        assert!(app.should_render_diff_for_active_editor());
        assert_eq!(app.compare_base_content.as_deref(), Some("unsaved left\n"));
        assert_eq!(app.editor().unwrap().rope.to_string(), "unsaved right\n");
        assert_eq!(app.compare_diff_lines, crate::app::compute_file_diff("unsaved left\n", "unsaved right\n"));
        assert_eq!(app.diff_labels, Some(crate::app::DiffLabels::new("left.txt", "right.txt")));
        assert_eq!(std::fs::read_to_string(&base).unwrap(), "saved left\n");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "saved right\n");

        // Selecting the next comparison must not change the one being displayed.
        choose(&mut app, 1, "tab_select_compare", keyboard);
        assert_eq!(app.selected_compare_path.as_ref(), Some(&target));
        assert_eq!(app.compare_base_path.as_ref(), Some(&base));
        assert_eq!(app.compare_base_content.as_deref(), Some("unsaved left\n"));
        app.clear_diff_state();
        assert_eq!(app.selected_compare_path.as_ref(), Some(&target));
        assert_eq!(app.compare_base_path, None);
    }
}

#[test]
fn explorer_new_file_click_and_hover_follow_the_rendered_menu() {
    use ratatui::{backend::TestBackend, Terminal};

    for (width, height, x, y, workspace_menu) in [
        (80, 40, 4, 3, true),
        (80, 40, 75, 36, true),
        (80, 24, 75, 17, false),
        (40, 18, 36, 8, true),
    ] {
        let workspace = tempfile::tempdir().expect("workspace");
        let mut app = App::new(workspace.path().to_path_buf());
        if workspace_menu {
            app.open_explorer_workspace_context_menu(x, y);
        } else {
            let folder = workspace.path().join("folder");
            std::fs::create_dir(&folder).unwrap();
            app.explorer.refresh();
            app.open_explorer_context_menu(x, y);
        }
        let action = app
            .explorer_context_menu_items()
            .iter()
            .position(|item| *item == "New File")
            .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui_dialogs::draw_explorer_context_menu(frame, &mut app))
            .unwrap();
        // Locate the actual rendered label, independently of the hit rectangle.
        let buffer = terminal.backend().buffer();
        let (col, row) = (0..height)
            .find_map(|row| {
                (0..width.saturating_sub(7)).find_map(|col| {
                    let label: String = (col..col + 8).map(|x| buffer[(x, row)].symbol()).collect();
                    (label == "New File").then_some((col, row))
                })
            })
            .expect("New File is visible");
        app.explorer_context_selected = usize::MAX;
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: col,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.explorer_context_selected, action);
        handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: col,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(!app.explorer_context_active);
        assert!(app.creating_new_file);
        assert!(app.rename.target.as_ref().unwrap().is_file());
    }
}

#[test]
fn handle_tab_click_close_clears_drag_state() {
    let mut app = App::new(PathBuf::from("."));
    app.tabs = vec![make_tab("a.rs"), make_tab("b.rs")];
    app.active_tab = 1;
    app.dragging_tab = Some(1);
    app.dragging_tab_source_group = Some(0);
    app.tab_drop_zone = Some(crate::app::TabDropZone::Left);
    app.tab_drop_target_group = Some(0);

    let tab_area = ratatui::layout::Rect::new(0, 0, 200, 1);
    let close_col = crate::ui_chrome_tabs::TabBarHits::new(&app,tab_area).tabs[1].2.x+1;

    assert!(handle_tab_click(&mut app, close_col, tab_area));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.dragging_tab, None);
    assert_eq!(app.dragging_tab_source_group, None);
    assert!(app.tab_drop_zone.is_none());
    assert_eq!(app.tab_drop_target_group, None);
}

#[test]
fn reconcile_dragging_tab_shifts_after_close_before_it() {
    let mut app = App::new(PathBuf::from("."));
    app.dragging_tab = Some(3);

    reconcile_dragging_tab_after_close(&mut app, 1);

    assert_eq!(app.dragging_tab, Some(2));
}

#[test]
fn activity_bar_switch_away_from_search_clears_global_search_popup() {
    let mut app = App::new(PathBuf::from("."));
    app.activity_bar_area = Some(Rect::new(0, 0, 2, 20));

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 3,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.global_search_active);
    assert!(app.show_explorer);
    assert_eq!(app.sidebar_view, crate::app::SidebarView::Search);

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(!app.global_search_active);
    assert!(app.show_explorer);
    assert_eq!(app.sidebar_view, crate::app::SidebarView::Explorer);
}

#[test]
fn activity_bar_toggling_search_off_clears_global_search_popup() {
    let mut app = App::new(PathBuf::from("."));
    app.activity_bar_area = Some(Rect::new(0, 0, 2, 20));

    let search_click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 3,
        modifiers: KeyModifiers::NONE,
    };

    handle_mouse(&mut app, search_click);
    assert!(app.global_search_active);
    assert!(app.show_explorer);
    assert_eq!(app.sidebar_view, crate::app::SidebarView::Search);

    handle_mouse(&mut app, search_click);
    assert!(!app.global_search_active);
    assert!(!app.show_explorer);
}

#[test]
fn activity_bar_toggling_source_control_off_hides_sidebar() {
    let mut app = App::new(PathBuf::from("."));
    app.activity_bar_area = Some(Rect::new(0, 0, 2, 20));

    let source_control_click = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 0,
        row: 6,
        modifiers: KeyModifiers::NONE,
    };

    handle_mouse(&mut app, source_control_click);
    assert!(app.show_explorer);
    assert_eq!(app.sidebar_view, crate::app::SidebarView::SourceControl);
    assert_eq!(app.activity_bar_selected, 2);

    handle_mouse(&mut app, source_control_click);
    assert!(!app.show_explorer);
    assert_eq!(app.sidebar_view, crate::app::SidebarView::SourceControl);
    assert_eq!(app.activity_bar_selected, 2);
}

#[test]
fn activity_item_hit_test_rejects_rows_outside_the_bar() {
    let area = Rect::new(0, 2, 5, 24);
    assert_eq!(activity_item_at(area, 1), None);
    assert_eq!(activity_item_at(area, 26), None);
    assert_eq!(activity_item_at(area, 2), Some(0));
    assert_eq!(activity_item_at(area, 8), Some(2));
    assert_eq!(activity_item_at(area, 11), Some(4));
    assert_eq!(activity_item_at(area, 14), Some(5));
    assert_eq!(activity_item_at(area, 17), None);
    assert_eq!(activity_item_at(area, 25), Some(ACTIVITY_SETTINGS_INDEX));

    let compact = Rect::new(0, 2, 5, 15);
    assert_eq!(
        activity_item_at(compact, 16),
        Some(ACTIVITY_SETTINGS_INDEX)
    );

    let taller = Rect::new(0, 2, 5, 17);
    assert_eq!(activity_item_at(taller, 18), Some(ACTIVITY_SETTINGS_INDEX));
}

#[test]
fn compact_menu_overflow_opens_the_first_hidden_menu() {
    let mut app = App::test_new();
    let area = Rect::new(0, 0, 40, 1);
    app.menu_bar_area = Some(area);
    let labels = App::menu_labels();
    let visible = crate::ui_chrome::visible_menu_label_count(area.width);
    assert!(visible < labels.len());
    let overflow_x = area.x
        + 3
        + labels
            .iter()
            .take(visible)
            .map(|label| unicode_width::UnicodeWidthStr::width(*label) as u16 + 2)
            .sum::<u16>();

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: overflow_x + 1,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(app.menu_active);
    assert!(app.menu_open);
    assert_eq!(app.menu_selected, visible);
}

#[test]
fn explorer_resize_handle_does_not_claim_sidebar_content_cells() {
    let area = Rect::new(5, 2, 30, 20);
    let border = area.x + area.width - 1;
    assert!(!explorer_resize_hit(area, false, border - 1, 10));
    assert!(explorer_resize_hit(area, false, border, 10));
    assert!(!explorer_resize_hit(area, false, border + 1, 10));

    assert!(explorer_resize_hit(area, true, area.x, 10));
    assert!(!explorer_resize_hit(area, true, area.x + 1, 10));
}

#[test]
fn explorer_file_click_keeps_delete_in_explorer_until_editor_is_focused() {
    for preview_enabled in [true, false] {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("example.txt");
        std::fs::write(&path, "sample\n").unwrap();
        let mut app = App::new(workspace.path().to_path_buf());
        app.enable_preview_tabs = preview_enabled;
        app.show_explorer = true;
        app.show_open_editors = false;
        app.sidebar_view = crate::app::SidebarView::Explorer;
        app.explorer_area = Some(Rect::new(2, 3, 30, 14));
        app.explorer.refresh();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 6,
            modifiers: KeyModifiers::NONE,
        };

        // Exercise both the initial background load and an already-open file.
        for _ in 0..2 {
            app.active_pane = ActivePane::Editor;
            app.last_click_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
            handle_mouse(&mut app, click);
            handle_mouse(
                &mut app,
                MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), ..click },
            );
            app.wait_for_background(std::time::Duration::from_secs(5), |app| {
                app.editor().is_some_and(|editor| !editor.bg_loading)
            });
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                event::KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
            );
            assert!(
                app.confirm_delete_active,
                "Delete must target the selected Explorer file",
            );
            assert_eq!(app.confirm_delete_target.as_ref(), Some(&path));
            assert_eq!(app.active_pane, ActivePane::Explorer);
            assert_eq!(app.editor().unwrap().rope.to_string(), "sample\n");
            assert!(!app.editor().unwrap().modified);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "sample\n");
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            );
            assert!(!app.confirm_delete_active);
        }

        // Explicitly opening the file still transfers focus and keeps its tab.
        if preview_enabled {
            app.last_click_time = std::time::Instant::now();
            app.click_count = 1;
            handle_mouse(&mut app, click);
            handle_mouse(
                &mut app,
                MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), ..click },
            );
        } else {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        }
        assert_eq!(app.active_pane, ActivePane::Editor);
        assert!(!app.tabs[app.active_tab].preview);

        // A later click in the editor gives Delete its ordinary text-editing role.
        app.active_pane = ActivePane::Explorer;
        app.code_area = Some(Rect::new(40, 5, 20, 4));
        let editor_click = MouseEvent { column: 40, row: 5, ..click };
        handle_mouse(&mut app, editor_click);
        handle_mouse(
            &mut app,
            MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), ..editor_click },
        );
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            event::KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
        );
        assert_eq!(app.active_pane, ActivePane::Editor);
        assert!(!app.confirm_delete_active);
        assert_eq!(app.editor().unwrap().rope.to_string(), "ample\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "sample\n");
    }
}

#[test]
fn explorer_whitespace_right_click_targets_an_empty_workspace() {
    let workspace = tempfile::tempdir().expect("empty workspace");
    let mut app = App::new(workspace.path().to_path_buf());
    assert!(app.explorer.entries.is_empty());

    let area = Rect::new(2, 3, 30, 14);
    app.show_explorer = true;
    app.show_open_editors = false;
    app.sidebar_view = crate::app::SidebarView::Explorer;
    app.explorer_area = Some(area);
    app.explorer_multi_selected = vec![workspace.path().join("stale-selection")].into();
    app.explorer_selection_anchor = Some(0);

    // With Open Editors collapsed, the tree begins three rows below the
    // sidebar top. Click a later, unused tree row rather than the root header.
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: area.x + 4,
            row: area.y + 5,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(app.explorer_context_active);
    assert_eq!(
        app.explorer_context_target.as_deref(),
        Some(workspace.path())
    );
    assert!(app.explorer_multi_selected.is_empty());
    assert!(app.explorer_selection_anchor.is_none());
    let items = app.explorer_context_menu_items();
    assert!(items.contains(&"New File"));
    assert!(items.contains(&"New Folder"));
    assert!(!items.contains(&"Rename"));
    assert!(!items.contains(&"Delete"));
}

fn render_search_controls(app: &mut App, area: Rect) {
    use ratatui::{backend::TestBackend, Terminal};
    let mut terminal = Terminal::new(TestBackend::new(area.right().max(80), area.bottom().max(24))).unwrap();
    terminal.draw(|frame| crate::ui_search_panel::draw_search_panel(frame, app, area)).unwrap();
}

#[test]
fn regex_toggle_click_does_not_start_sidebar_resize() {
    let mut app = App::new(PathBuf::from("."));
    let area = Rect::new(0, 0, 30, 20);
    app.explorer_area = Some(area);
    app.sidebar_view = crate::app::SidebarView::Search;
    app.show_explorer = true;
    render_search_controls(&mut app, area);
    let regex = crate::ui_search_panel::search_toggle_areas(area)[2];

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: regex.x + regex.width - 1,
            row: regex.y,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(app.search_regex);
    assert!(!app.resizing_explorer);
}

#[test]
fn open_editor_close_affordance_is_at_the_right_edge() {
    let area = Rect::new(5, 1, 30, 20);
    assert!(!open_editor_close_hit(area, 5));
    assert!(!open_editor_close_hit(area, 31));
    assert!(open_editor_close_hit(area, 32));
    assert!(open_editor_close_hit(area, 34));
    assert!(!open_editor_close_hit(area, 35));
}

#[test]
fn search_collapse_button_remains_clickable_in_narrow_sidebar() {
    let mut app = App::new(PathBuf::from("."));
    let area = Rect::new(0, 10, 12, 20);
    app.show_explorer = true;
    app.sidebar_view = crate::app::SidebarView::Search;
    app.explorer_area = Some(area);
    app.global_search_results = vec![crate::app::GlobalSearchResult {
        file_path: PathBuf::from("src\\main.rs"),
        line_num: 1,
        line_text: "fn main() {}".into(),
        col_start: 0,
        col_end: 2,
        context_before: None,
        context_after: None,
    }].into();

    render_search_controls(&mut app, area);
    let collapse = crate::ui_search_panel::search_header_action_areas(area)[2];
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: collapse.x,
            row: collapse.y,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(app.all_global_search_files_collapsed());
}

#[test]
fn search_refresh_button_restarts_the_current_search() {
    let mut app = App::new(PathBuf::from("."));
    let area = Rect::new(0, 10, 30, 20);
    app.show_explorer = true;
    app.sidebar_view = crate::app::SidebarView::Search;
    app.explorer_area = Some(area);
    app.bg_global_search_running = true;
    render_search_controls(&mut app, area);
    let refresh = crate::ui_search_panel::search_header_action_areas(area)[0];

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: refresh.x,
            row: refresh.y,
            modifiers: KeyModifiers::NONE,
        },
    );

    // An empty current query is refreshed into the idle state. This proves
    // the header icon is a real action rather than decorative chrome.
    assert!(!app.bg_global_search_running);
}

#[test]
fn search_clear_button_clears_query_results_and_filters_nothing_else() {
    let mut app = App::new(PathBuf::from("."));
    let area = Rect::new(0, 10, 30, 20);
    app.show_explorer = true;
    app.sidebar_view = crate::app::SidebarView::Search;
    app.explorer_area = Some(area);
    app.global_search_query = "needle".into();
    app.global_search_field_cursors[0] = app.global_search_query.len();
    app.search_include_pattern = "src/**".into();
    app.global_search_results = vec![crate::app::GlobalSearchResult {
        file_path: PathBuf::from("src/main.rs"),
        line_num: 1,
        line_text: "needle".into(),
        col_start: 0,
        col_end: 6,
        context_before: None,
        context_after: None,
    }].into();
    render_search_controls(&mut app, area);
    let clear = crate::ui_search_panel::search_header_action_areas(area)[1];

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: clear.x,
            row: clear.y,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(app.global_search_query.is_empty());
    assert!(app.global_search_results.is_empty());
    assert_eq!(app.global_search_field_cursors[0], 0);
    assert_eq!(app.search_include_pattern, "src/**");
}

#[test]
fn workspace_search_result_drag_never_moves_the_editor_caret() {
    use ratatui::{backend::TestBackend, Terminal};

    for sidebar_right in [false, true] {
        for unfinished_editor_gesture in [false, true] {
            let mut app = App::test_new();
            app.sidebar_right = sidebar_right;
            app.show_explorer = true;
            app.sidebar_view = crate::app::SidebarView::Search;
            app.show_panel = false;
            app.show_minimap = false;
            app.chat.visible = false;
            let path = app.explorer.root.join("search-drag-fixture.txt");
            let text = "row needle and some more text\n".repeat(80);
            let mut editor = Editor::test_with_text(&text);
            editor.file_path = Some(path.clone());
            app.tabs.push(Tab::new_file(editor));
            app.active_tab = 0;
            app.global_search_query = "needle".into();
            app.global_search_results = vec![crate::app::GlobalSearchResult {
                file_path: path, line_num: 8, line_text: "row needle and some more text".into(),
                col_start: 4, col_end: 10, context_before: None, context_after: None,
            }].into();
            let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let pointer = |kind, column, row| MouseEvent {
                kind, column, row, modifiers: KeyModifiers::NONE,
            };
            if unfinished_editor_gesture {
                let code = app.code_area.unwrap();
                handle_mouse(&mut app, pointer(MouseEventKind::Down(MouseButton::Left), code.x + 2, code.y + 1));
            }
            let sidebar = app.explorer_area.unwrap();
            let result_y = crate::ui_search_panel::search_results_start_y(&app, sidebar) + 1;
            handle_mouse(&mut app, pointer(MouseEventKind::Down(MouseButton::Left), sidebar.x + 3, result_y));
            assert_eq!(app.active_pane, ActivePane::Editor);
            assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (7, 4));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let state = |app: &App| {
                let editor = app.editor().unwrap();
                (editor.cursor_line, editor.cursor_col, editor.selection_anchor,
                    editor.scroll_offset, editor.wrap_scroll_offset, editor.horizontal_scroll)
            };
            let before = state(&app);
            let code = app.code_area.unwrap();
            for (column, row) in [
                (sidebar.x + 4, result_y + 2),
                (code.x + 6, code.y + 3),
                (code.x + 6, code.y.saturating_sub(1)),
                (code.x + 6, code.bottom() + 1),
            ] {
                handle_mouse(&mut app, pointer(MouseEventKind::Drag(MouseButton::Left), column, row));
                assert_eq!(state(&app), before, "sidebar_right={sidebar_right}, unfinished_editor_gesture={unfinished_editor_gesture}, pointer={column}:{row}");
            }
            handle_mouse(&mut app, pointer(MouseEventKind::Up(MouseButton::Left), code.x + 6, code.y + 3));
            assert_eq!(state(&app), before);

            // An ordinary editor press still begins a real selection afterwards.
            handle_mouse(&mut app, pointer(MouseEventKind::Down(MouseButton::Left), code.x + 2, code.y + 1));
            handle_mouse(&mut app, pointer(MouseEventKind::Drag(MouseButton::Left), code.x + 8, code.y + 2));
            assert!(app.editor().unwrap().selection_range().is_some());
            handle_mouse(&mut app, pointer(MouseEventKind::Up(MouseButton::Left), code.x + 8, code.y + 2));
            let selected = state(&app);
            handle_mouse(&mut app, pointer(MouseEventKind::Drag(MouseButton::Left), code.x + 12, code.y + 4));
            assert_eq!(state(&app), selected, "a released gesture cannot extend the selection");
        }
    }
}

#[test]
fn context_menu_stays_open_on_mouse_release_outside_menu() {
    let mut app = App::new(PathBuf::from("."));
    app.open_source_control_action_menu(10, 5);

    handle_context_menu_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 10,
            row: 4,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(app.context_menu_active);
}

#[test]
fn context_menu_closes_on_mouse_down_outside_menu() {
    let mut app = App::new(PathBuf::from("."));
    app.open_source_control_action_menu(10, 5);

    handle_context_menu_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 50,
            row: 15,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(!app.context_menu_active);
}

#[test]
fn context_menu_closes_after_selecting_item() {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("select this"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.context_menu_active = true;
    app.context_menu_pos = (10, 5);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
    terminal.draw(|frame| crate::ui_dialogs::draw_context_menu(frame, &mut app)).unwrap();
    let inner = app.context_menu_inner.unwrap();

    handle_context_menu_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: inner.x + 1,
            row: inner.y + 4, // Select All, after the Paste separator.
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(!app.context_menu_active);
    assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("select this"));
}

#[test]
fn tab_click_clears_stale_diff_state_for_other_file() {
    let mut app = App::new(PathBuf::from("."));
    app.tabs = vec![make_tab("a.rs"), make_tab("b.rs")];
    app.active_tab = 0;
    app.diff_mode = crate::app::DiffMode::SideBySide;
    app.compare_base_content = Some("before\n".to_string().into());
    app.compare_diff_lines = crate::app::compute_file_diff("before\n", "after\n");
    app.diff_target_path = Some(PathBuf::from("a.rs"));

    let tab_area = ratatui::layout::Rect::new(0, 0, 200, 1);
    let second_tab_col = crate::ui_chrome_tabs::TabBarHits::new(&app,tab_area).tabs[1].1.x;

    assert!(handle_tab_click(&mut app, second_tab_col, tab_area));
    assert_eq!(app.active_tab, 1);
    assert!(matches!(app.diff_mode, crate::app::DiffMode::Off));
    assert!(app.compare_base_content.is_none());
    assert!(app.diff_target_path.is_none());
}

#[test]
fn moving_from_tab_into_editor_clears_path_tooltip() {
    let mut app = App::new(PathBuf::from("."));
    app.tabs = vec![make_tab("src/main.rs")];
    app.active_tab = 0;
    app.tab_bar_area = Some(Rect::new(0, 0, 80, 1));
    app.code_area = Some(Rect::new(0, 1, 80, 20));

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 6,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert_eq!(app.tab_tooltip_tab, Some(0));
    assert!(app.tab_tooltip_hover_start.is_some());

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: 2,
            row: 2,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert_eq!(app.hovered_tab, None);
    assert_eq!(app.tab_tooltip_tab, None);
    assert!(app.tab_tooltip_hover_start.is_none());
}

#[test]
fn alt_click_gesture_toggles_only_on_mouse_down() {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("abcdef\n");
    editor.file_path = Some(PathBuf::from("toggle-cursor.rs"));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.code_area = Some(Rect::new(10, 5, 20, 4));
    let alt_down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 13,
        row: 5,
        modifiers: KeyModifiers::ALT,
    };

    handle_mouse(&mut app, alt_down);
    assert_eq!(app.editor().expect("editor").extra_cursors, vec![(0, 3)]);
    assert!(app.alt_cursor_mouse_down_active);

    // A terminal may report slight movement between press and release. Neither
    // that drag nor the release may move the primary caret or add a cursor.
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 16,
            row: 5,
            modifiers: KeyModifiers::ALT,
        },
    );
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 16,
            row: 5,
            modifiers: KeyModifiers::ALT,
        },
    );

    let editor = app.editor().expect("editor");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 0));
    assert_eq!(editor.extra_cursors, vec![(0, 3)]);
    assert!(!app.alt_cursor_mouse_down_active);

    // A second press at the same position removes the cursor; its release is inert.
    handle_mouse(&mut app, alt_down);
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 13,
            row: 5,
            modifiers: KeyModifiers::ALT,
        },
    );
    let editor = app.editor().expect("editor");
    assert!(editor.extra_cursors.is_empty());
    assert!(editor.extra_cursor_anchors.is_empty());
}

fn selection_app() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("first word here\nsecond row\nthird row\n");
    editor.cursor_col = 6;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.code_area = Some(Rect::new(10, 5, 30, 6));
    app
}

fn selection_mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) {
    handle_mouse(app, MouseEvent { kind, column, row, modifiers });
}

#[test]
fn selection_drag_does_not_transfer_to_a_different_editor() {
    let mut app = selection_app();
    selection_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 12, 6, KeyModifiers::NONE);
    app.tabs.push(Tab::new_file(Editor::test_with_text("another document\nsecond line\n")));
    app.active_tab = 1;
    let before = (app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col,
        app.editor().unwrap().selection_anchor);
    selection_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 17, 7, KeyModifiers::NONE);
    let editor = app.editor().unwrap();
    assert_eq!((editor.cursor_line, editor.cursor_col, editor.selection_anchor), before);
}

fn selection_double_click(app: &mut App, column: u16, row: u16) {
    app.last_click_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
    selection_mouse(app, MouseEventKind::Down(MouseButton::Left), column, row, KeyModifiers::NONE);
    selection_mouse(app, MouseEventKind::Up(MouseButton::Left), column, row, KeyModifiers::NONE);
    selection_mouse(app, MouseEventKind::Down(MouseButton::Left), column, row, KeyModifiers::NONE);
}

#[test]
fn double_click_selects_whitespace_runs_without_line_endings() {
    for (text, positions, expected) in [
        ("   word\n", vec![0, 1, 2], Some("   ")),
        ("one   two\n", vec![3, 4, 5], Some("   ")),
        ("one two\n", vec![3], Some(" ")),
        ("word  \r\nnext", vec![4, 5, 6, 12], Some("  ")),
        (" \t \t\r\nnext", vec![0, 1, 2, 3, 4, 12], Some(" \t \t")),
        ("word\t\tend", vec![4, 5], Some("\t\t")),
        ("a\u{a0}\u{a0}b", vec![1, 2], Some("\u{a0}\u{a0}")),
        ("   ", vec![0, 1, 2, 3, 12], Some("   ")),
        ("\nnext", vec![0, 5], None),
        ("\r\nnext", vec![0, 5], None),
        ("", vec![0, 5], None),
        ("one two", vec![0, 1, 2], Some("one")),
        ("one two", vec![4, 5, 6, 7, 12], Some("two")),
        ("word)", vec![4], Some("word")),
    ] {
        for col in positions {
            let mut app = selection_app();
            app.tabs[0].editor = Editor::test_with_text(text);
            selection_double_click(&mut app, 10 + col, 5);
            selection_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 10 + col, 5, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().selected_text().as_deref(), expected, "{text:?}, column {col}");
            assert_eq!(app.editor().unwrap().rope.to_string(), text);
        }
    }
}

#[test]
fn double_click_whitespace_drag_keeps_whole_runs_in_both_directions() {
    let mut app = selection_app();
    app.tabs[0].editor = Editor::test_with_text("one   two   three\n");
    selection_double_click(&mut app, 14, 5);
    for (col, expected) in [
        (15, "   "),
        (17, "   two"),
        (20, "   two   "),
        (11, "one   "),
        (14, "   "),
    ] {
        selection_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), col, 5, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some(expected), "column {col}");
    }
    selection_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 14, 5, KeyModifiers::NONE);
    assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("   "));
    assert_eq!(app.editor().unwrap().rope.to_string(), "one   two   three\n");
}

#[test]
fn double_click_whitespace_selection_can_be_deleted_and_undone() {
    let mut app = selection_app();
    app.tabs[0].editor = Editor::test_with_text("one   two\n");
    selection_double_click(&mut app, 13, 5);
    selection_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 13, 5, KeyModifiers::NONE);
    crate::input_keyboard::handle_keyboard_event(
        &mut app, event::KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), "onetwo\n");
    crate::input_keyboard::handle_keyboard_event(
        &mut app, event::KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), "one   two\n");
}

#[test]
fn shift_click_extends_from_caret_and_keeps_the_original_anchor() {
    let mut app = selection_app();
    for (column, row, cursor) in [(16, 6, (1, 6)), (12, 5, (0, 2)), (14, 7, (2, 4))] {
        selection_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), column, row, KeyModifiers::SHIFT);
        selection_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), column, row, KeyModifiers::SHIFT);
        let ed = app.editor().unwrap();
        assert_eq!(ed.selection_anchor, Some((0, 6)));
        assert_eq!((ed.cursor_line, ed.cursor_col), cursor);
    }
    assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("word here\nsecond row\nthir"));
}

#[test]
fn shift_drag_after_word_selection_uses_exact_character_endpoints() {
    let mut app = selection_app();
    for _ in 0..2 {
        selection_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 17, 5, KeyModifiers::NONE);
        selection_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 17, 5, KeyModifiers::NONE);
    }
    assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("word"));
    selection_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 13, 6, KeyModifiers::SHIFT);
    // Releasing Shift before the mouse must keep the same drag anchor.
    selection_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 12, 7, KeyModifiers::NONE);
    selection_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 12, 7, KeyModifiers::NONE);
    assert_eq!(app.editor().unwrap().selection_anchor, Some((0, 6)));
    assert_eq!(app.editor().unwrap().selected_text().as_deref(), Some("word here\nsecond row\nth"));
    assert!(app.multi_click_anchor.is_none());
    assert!(!app.dragging_text);
}

#[test]
fn shift_click_inside_selection_extends_without_moving_text_or_extra_cursors() {
    let mut app = selection_app();
    let ed = app.editor_mut().unwrap();
    ed.selection_anchor = Some((0, 1));
    ed.extra_cursors = vec![(1, 2)];
    ed.extra_cursor_anchors = vec![None];
    selection_mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 13, 5, KeyModifiers::SHIFT);
    selection_mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 15, 6, KeyModifiers::SHIFT);
    selection_mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 15, 6, KeyModifiers::SHIFT);
    let ed = app.editor().unwrap();
    assert_eq!(ed.selection_anchor, Some((0, 1)));
    assert_eq!(ed.selected_text().as_deref(), Some("irst word here\nsecon"));
    assert_eq!(ed.rope.to_string(), "first word here\nsecond row\nthird row\n");
    assert!(ed.extra_cursors.is_empty());
}

#[test]
fn shift_wheel_moves_horizontal_scroll_without_moving_caret_or_vertical_scroll() {
    let mut app = selection_app();
    app.cached_max_line_width = 200;
    let ed = app.editor_mut().unwrap();
    ed.viewport_width = 30;
    ed.word_wrap = false;
    for (kind, expected) in [(MouseEventKind::ScrollDown, 6), (MouseEventKind::ScrollDown, 12), (MouseEventKind::ScrollUp, 6)] {
        selection_mouse(&mut app, kind, 20, 6, KeyModifiers::SHIFT);
        let ed = app.editor().unwrap();
        assert_eq!(ed.horizontal_scroll, expected);
        assert_eq!(ed.scroll_offset, 0);
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 6));
    }
}

#[test]
fn alt_clicking_the_primary_cursor_promotes_a_secondary_cursor() {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("abcdef\n");
    editor.file_path = Some(PathBuf::from("promote-cursor.rs"));
    editor.extra_cursors = vec![(0, 3), (0, 5)];
    editor.extra_cursor_anchors = vec![None, Some((0, 4))];
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.code_area = Some(Rect::new(10, 5, 20, 4));

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::ALT,
        },
    );

    let editor = app.editor().expect("editor");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 5));
    assert_eq!(editor.selection_anchor, Some((0, 4)));
    assert_eq!(editor.extra_cursors, vec![(0, 3)]);
    assert_eq!(editor.extra_cursor_anchors, vec![None]);
}

#[test]
fn clicking_the_editor_leaves_find_visible_but_releases_its_focus() {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("abcdef\n");
    editor.file_path = Some(PathBuf::from("find-focus.rs"));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = crate::app::ActivePane::Editor;
    app.code_area = Some(Rect::new(10, 5, 20, 4));
    app.search_active = true;
    app.search_input_focused = true;
    app.search_pre_cursor = Some((0, 0, 0));

    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 13,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
    );

    assert!(app.search_active);
    assert!(!app.search_input_focused);
    assert_eq!(app.search_pre_cursor, None);
    assert_eq!(app.editor().expect("editor").cursor_col, 3);
}

#[test]
fn settings_category_tree_mouse_rows_follow_expanded_branch() {
    let mut app = App::new(PathBuf::from("."));
    app.settings_editor_active = true;
    app.active_pane = crate::app::ActivePane::Editor;
    app.settings_category_selected = 2;
    app.settings_category_collapsed[2] = false;
    app.settings_category_area = Some(Rect::new(0, 0, 30, 12));

    // The expanded Workbench child occupies visual row 3.
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 3,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.settings_category_selected, 2);
    assert_eq!(app.settings_selected_section(2), Some("Appearance"));

    // Features follows all three Workbench subpages at visual row 6.
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 6,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.settings_category_selected, 3);
    assert!(!app.settings_category_collapsed[3]);

    // Clicking the active root's chevron collapses only its nav child.
    handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 6,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.settings_category_collapsed[3]);
    assert!(!app.settings_filtered_indices_for_category(3).is_empty());
}

// --- tab_render_width tests ---

#[test]
fn tab_render_width_basic() {
    let app = App::test_new();
    let tab = make_tab("main.rs");
    let w = tab_render_width(&app, 0, &tab);
    assert!(w > 5, "tab should have reasonable width, got {w}");
}

#[test]
fn tab_render_width_modified_wider() {
    let app = App::test_new();
    let tab = make_tab("main.rs");
    let mut modified_tab = make_tab("main.rs");
    modified_tab.editor.modified = true;
    let w_normal = tab_render_width(&app, 0, &tab);
    let w_modified = tab_render_width(&app, 0, &modified_tab);
    assert!(w_modified > w_normal, "modified tab should be wider");
}

#[test]
fn tab_render_width_pinned_wider() {
    let app = App::test_new();
    let tab = make_tab("main.rs");
    let mut pinned_tab = make_tab("main.rs");
    pinned_tab.pinned = true;
    let w_normal = tab_render_width(&app, 0, &tab);
    let w_pinned = tab_render_width(&app, 0, &pinned_tab);
    assert!(w_pinned > w_normal, "pinned tab should be wider");
}

#[test]
fn tab_render_width_longer_name_wider() {
    let app = App::test_new();
    let short_tab = make_tab("a.rs");
    let long_tab = make_tab("very_long_filename.rs");
    let w_short = tab_render_width(&app, 0, &short_tab);
    let w_long = tab_render_width(&app, 0, &long_tab);
    assert!(w_long > w_short, "longer filename should produce wider tab");
}

// --- tab_display_name tests ---

#[test]
fn tab_display_name_normal_file() {
    let app = App::test_new();
    let tab = make_tab("main.rs");
    let name = tab_display_name(&app, 0, &tab);
    assert_eq!(&*name, "main.rs");
}

#[test]
fn tab_display_name_markdown_preview() {
    let app = App::test_new();
    let mut tab = make_tab("readme.md");
    tab.kind = TabKind::MarkdownPreview;
    let name = tab_display_name(&app, 0, &tab);
    assert_eq!(&*name, "Preview: readme.md");
}

// --- tab_bar_right_indicator_start tests ---

#[test]
fn tab_bar_no_indicator_when_all_fit() {
    let mut app = App::test_new();
    app.tabs = vec![make_tab("a.rs")];
    app.active_tab = 0;
    let area = ratatui::layout::Rect::new(0, 0, 200, 1);
    assert!(tab_bar_right_indicator_start(&app, area).is_none());
}

#[test]
fn tab_bar_indicator_when_tabs_overflow() {
    let mut app = App::test_new();
    for i in 0..50 {
        app.tabs
            .push(make_tab(&format!("file_{i}_with_long_name.rs")));
    }
    app.active_tab = 0;
    let area = ratatui::layout::Rect::new(0, 0, 80, 1);
    assert!(tab_bar_right_indicator_start(&app, area).is_some());
}

// --- ensure_group_active_tab_visible tests ---

#[test]
fn ensure_group_visible_empty() {
    let mut group = crate::app::EditorGroup::default();
    ensure_group_active_tab_visible(&mut group);
    assert_eq!(group.tab_scroll_offset, 0);
}

#[test]
fn ensure_group_visible_resets_scroll_for_active() {
    let mut group = crate::app::EditorGroup::default();
    group.tabs.push(make_tab("a.rs"));
    group.tabs.push(make_tab("b.rs"));
    group.active_tab = 0;
    group.tab_scroll_offset = 5;
    ensure_group_active_tab_visible(&mut group);
    assert_eq!(group.tab_scroll_offset, 0);
}
