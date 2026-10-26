use super::*;
use crate::app::{SidebarView, SourceControlRepositoryState as State};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture() -> App {
    let mut app = App::test_new();
    app.show_explorer = true;
    app.sidebar_view = SidebarView::SourceControl;
    app.source_control_repository_state = State::Available;
    app.sc_graph_collapsed = true;
    app
}

fn text(buffer: &Buffer, area: Rect) -> String {
    (area.y..area.bottom()).map(|y| {
        (area.x..area.right()).map(|x| buffer[(x, y)].symbol()).collect::<String>()
    }).collect::<Vec<_>>().join("\n")
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    let event = MouseEvent {
        kind, column: x, row: y, modifiers: KeyModifiers::NONE,
    };
    // Match the event loop: an open dropdown owns pointer input.
    if app.context_menu_active { crate::mouse_overlay::handle_context_menu_mouse(app, event); }
    else { crate::mouse::handle_mouse(app, event); }
}

fn visual_row(app: &App, index: usize) -> usize {
    index + app.context_menu_items[..index].iter().filter(|item| item.separator_after).count()
}

#[test]
fn single_actions_control_stays_visible_in_every_sidebar_state_and_width() {
    for width in [1, 2, 3, 8, 15, 24, 40, 80] {
        for right in [false, true] {
            for state in [State::Checking, State::Loading, State::Missing, State::Available, State::Disabled] {
                let mut app = fixture();
                app.sidebar_right = right;
                app.source_control_enabled = state != State::Disabled;
                app.source_control_repository_state = state;
                let area = Rect::new(5, 1, width, 19);
                app.explorer_area = Some(area);
                let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
                terminal.draw(|f| draw_source_control_sidebar(f, &mut app, area)).unwrap();
                let button = app.source_control_actions_area.expect("actions always fit");
                assert!(area.contains((button.x, button.y).into()));
                assert!(button.right() <= area.right());
                let header = text(terminal.backend().buffer(), Rect::new(area.x, area.y, area.width, 1));
                assert!(!header.contains("Source Control"));
                assert!(header.contains(if button.width >= 11 { "Actions" } else { "⋮" }), "{header:?}");
                if state != State::Disabled { assert!(app.source_control_toggle_area.is_none()); }
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), button.x, button.y);
                assert!(app.source_control_action_menu_open(), "{state:?}, {width}, right={right}");
                assert_eq!(app.context_menu_items.last().unwrap().id, "source_control_toggle");
            }
        }
    }
}

#[test]
fn grouped_actions_menu_separators_are_not_clickable_and_rows_map_to_commands() {
    for anchor in [(4, 2), (78, 22)] {
        let mut app = fixture();
        app.open_source_control_action_menu(anchor.0, anchor.1);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
        let inner = app.context_menu_inner.unwrap();
        let rendered = text(terminal.backend().buffer(), inner);
        for label in ["Refresh", "Fetch", "Pull", "Push", "Branch", "Show untracked files", "Disable Source Control"] {
            assert!(rendered.contains(label), "{label}: {rendered}");
        }
        for index in 0..app.context_menu_items.len() {
            let row = inner.y + visual_row(&app, index) as u16;
            mouse(&mut app, MouseEventKind::Moved, inner.x + 2, row);
            assert_eq!(app.context_menu_selected, index);
            if app.context_menu_items[index].separator_after {
                assert!(terminal.backend().buffer()[(inner.x + 1, row + 1)].symbol().contains('─'));
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), inner.x + 2, row + 1);
                assert!(app.context_menu_active);
                assert_eq!(app.context_menu_selected, index);
                assert!(!app.bg_git_remote_running && !app.bg_git_action_running);
            }
        }
        crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Esc);
        assert!(!app.context_menu_active);
    }
}

#[test]
fn short_actions_menu_scrolls_to_selection_and_filter_clicks_keep_the_same_menu() {
    for height in [6, 8, 12, 24] {
        for anchor in [(0, 1), (39, height - 1)] {
            let mut app = fixture();
            app.open_source_control_action_menu(anchor.0, anchor.1);
            let mut terminal = Terminal::new(TestBackend::new(40, height)).unwrap();
            for _ in 1..app.context_menu_items.len() {
                terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
                let inner = app.context_menu_inner.unwrap();
                mouse(&mut app, MouseEventKind::ScrollDown, inner.x + 1, inner.y);
            }
            terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
            let inner = app.context_menu_inner.unwrap();
            assert!(text(terminal.backend().buffer(), inner).contains("Disable Source Control"));
            if height < 14 { assert!(app.context_menu_scroll > 0); }
            crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Up);
            terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
            let inner = app.context_menu_inner.unwrap();
            let index = app.context_menu_selected;
            assert_eq!(app.context_menu_items[index].id, "git_show_untracked");
            let row = inner.y + (visual_row(&app, index) - app.context_menu_scroll) as u16;
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), inner.x + 2, row);
            assert!(!app.sc_visibility.untracked);
            assert!(app.source_control_action_menu_open());
            assert_eq!(app.context_menu_selected, index);
            assert!(!app.context_menu_items[index].label.starts_with('✓'));
            terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
            crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Char(' '));
            assert!(app.sc_visibility.untracked);
            assert!(app.source_control_action_menu_open());
            assert_eq!(app.context_menu_selected, index);
            crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Home);
            terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
            assert_eq!(app.context_menu_scroll, 0);
            assert!(text(terminal.backend().buffer(), app.context_menu_inner.unwrap()).contains("Refresh"));
        }
    }
}

#[test]
fn unavailable_repository_operations_are_dimmed_and_cannot_run_from_menu() {
    for state in [State::Checking, State::Loading, State::Missing, State::Available] {
        let mut app = fixture();
        app.source_control_repository_state = state;
        if state == State::Available { app.bg_git_remote_running = true; }
        app.open_source_control_action_menu(3, 1);
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
        let inner = app.context_menu_inner.unwrap();
        for index in 1..=4 {
            let row = inner.y + visual_row(&app, index) as u16;
            assert_ne!(terminal.backend().buffer()[(inner.x + 1, row)].fg,
                terminal.backend().buffer()[(inner.x + 1, inner.y)].fg);
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), inner.x + 2, row);
            assert!(app.context_menu_active);
            crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Enter);
            assert!(app.context_menu_active);
            assert!(!app.bg_git_branch_picker_running && !app.bg_git_action_running);
            assert_eq!(app.bg_git_remote_running, state == State::Available);
        }
    }
}

#[test]
fn actions_button_owns_press_drag_release_across_button_gap_and_flipped_menu() {
    for width in [1, 11, 40] {
        for right in [false, true] {
            for y in [1, 24] {
                let root = tempfile::tempdir().unwrap();
                let mut app = fixture();
                app.explorer = crate::explorer::FileExplorer::new_deferred(root.path().to_owned());
                app.sidebar_right = right;
                app.tabs = vec![crate::app::Tab::new_file(crate::editor::Editor::test_with_text("alpha\nbeta\ngamma\n"))];
                app.active_tab = 0;
                app.editor_mut().unwrap().selection_anchor = Some((0, 1));
                app.editor_mut().unwrap().cursor_line = 1;
                app.editor_mut().unwrap().cursor_col = 2;
                let area = Rect::new(if right { 98 - width } else { 2 }, y, width, 32 - y);
                app.explorer_area = Some(area);
                let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
                terminal.draw(|f| draw_source_control_sidebar(f, &mut app, area)).unwrap();
                let button = app.source_control_actions_area.unwrap();
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), button.x, button.y);
                assert!(app.drag_start_pos.is_none());
                assert_eq!(app.context_menu_button_drag, Some(button));

                // Terminals can report a held move before the first menu frame.
                mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), button.x, button.y);
                assert!(app.context_menu_active);
                terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
                let inner = app.context_menu_inner.unwrap();
                let index = app.context_menu_items.iter().position(|item| item.id == "source_control_toggle").unwrap();
                let target_y = inner.y + (visual_row(&app, index) - app.context_menu_scroll) as u16;
                assert!(target_y < inner.bottom());
                for (x, y) in [(button.x, button.y), (inner.x, inner.y - 1), (50, 31), (inner.x + 1, target_y)] {
                    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, y);
                    assert!(app.context_menu_active && app.source_control_enabled);
                }
                assert_eq!(app.context_menu_selected, index);
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), inner.x + 1, target_y);
                assert!(!app.context_menu_active && !app.source_control_enabled);
                assert!(app.context_menu_button_drag.is_none());
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), inner.x + 1, target_y);
                assert!(!app.source_control_enabled, "a repeated release cannot run the command twice");
                let editor = app.editor().unwrap();
                assert_eq!((editor.cursor_line, editor.cursor_col, editor.selection_anchor), (1, 2, Some((0, 1))));
                assert_eq!(editor.rope.to_string(), "alpha\nbeta\ngamma\n");
            }
        }
    }
}

#[test]
fn button_releases_separators_disabled_rows_and_cancelled_gestures_do_not_activate_commands() {
    let mut app = fixture();
    let area = Rect::new(2, 1, 40, 26);
    app.explorer_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
    terminal.draw(|f| draw_source_control_sidebar(f, &mut app, area)).unwrap();
    let button = app.source_control_actions_area.unwrap();
    for release in 0..5 {
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), button.x, button.y);
        terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
        let inner = app.context_menu_inner.unwrap();
        let (x, y) = match release {
            0 => (button.x, button.y),
            1 => (inner.x, inner.y - 1), // Border.
            2 => (inner.x + 1, inner.y + 1), // Separator after Refresh.
            3 => (99, 31), // Outside the menu.
            _ => {
                app.bg_git_remote_running = true;
                (inner.x + 1, inner.y + visual_row(&app, 1) as u16) // Disabled Fetch.
            }
        };
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
        assert!(app.source_control_action_menu_open());
        assert!(app.context_menu_button_drag.is_none());
        assert!(!app.bg_git_action_running && !app.bg_git_branch_picker_running);
        let index = app.context_menu_items.len() - 1;
        let command_y = inner.y + visual_row(&app, index) as u16;
        // A release-only event cannot turn the subsequent click into a command.
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), inner.x + 1, command_y);
        assert!(app.source_control_enabled);
        crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Esc);
    }

    // Keyboard navigation takes over a held gesture; its late release is inert.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), button.x, button.y);
    terminal.draw(|f| crate::ui_dialogs::draw_context_menu(f, &mut app)).unwrap();
    let inner = app.context_menu_inner.unwrap();
    let index = app.context_menu_items.len() - 1;
    let command_y = inner.y + visual_row(&app, index) as u16;
    crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::End);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), inner.x + 1, command_y);
    assert!(app.source_control_enabled && app.context_menu_active);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 99, 31);
    assert!(!app.context_menu_active && app.context_menu_button_drag.is_none());
}

#[test]
fn actions_and_reopen_buttons_have_hover_styles_without_tooltips() {
    for untracked in [false, true] {
        let mut app = fixture();
        app.sc_visibility.untracked = untracked;
        let closed = app.explorer.root.join("closed-project");
        app.repositories.parent = Some(closed.clone());
        app.repositories.closed.insert(closed);
        let area = Rect::new(2, 1, 60, 26);
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal.draw(|f| draw_source_control_sidebar(f, &mut app, area)).unwrap();
        for button in [app.source_control_actions_area.unwrap(), app.repositories.reopen_button.unwrap()] {
            mouse(&mut app, MouseEventKind::Moved, button.x, button.y);
            terminal.draw(|f| {
                draw_source_control_sidebar(f, &mut app, area);
                let before = f.buffer_mut().clone();
                draw_action_tooltip(f, &app);
                assert_eq!(&*f.buffer_mut(), &before, "header dropdowns must not paint a tooltip");
            }).unwrap();
        }
    }
}
