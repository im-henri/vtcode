use super::*;
use crate::text_input::TextInputId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("unchanged document"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.terminals = ["First", "Second", "Third"].into_iter().map(|name| {
        let mut terminal = TerminalTab::new(name.into(), app.explorer.root.clone());
        terminal.terminal_launch_error = Some("Not launched for rendering tests".into());
        terminal
    }).collect();
    app.active_terminal = 0;
    app.show_terminal_sidebar = true;
    app.terminal_sidebar_revealed = true;
    app.terminal_panel_focused = true;
    app
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn draw(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal
}

fn right_click(app: &mut App, column: u16, row: u16) {
    crate::mouse::handle_mouse(app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Right), column, row, modifiers: KeyModifiers::NONE,
    });
}

#[test]
fn terminal_rename_opens_from_palette_menu_and_both_tab_context_menus() {
    for route in 0..4 {
        let mut app = fixture();
        if route == 3 {
            app.detach_terminal_to_group(1, app.active_group);
            app.active_tab = 0;
            app.active_terminal = 0;
            app.terminal_panel_focused = false;
        }
        draw(&mut app);
        match route {
            0 => {
                app.active_terminal = 1;
                app.execute_palette_command("rename_terminal");
            }
            1 => {
                app.active_terminal = 1;
                app.menu_active = true;
                app.menu_open = true;
                app.menu_selected = 6;
                app.menu_item_selected = app.menu_items(6).iter()
                    .position(|(_, id)| *id == "rename_terminal").unwrap();
                key(&mut app, KeyCode::Enter);
            }
            2 => {
                let area = app.terminal_sidebar_tab_areas.iter()
                    .find(|(_, index)| *index == 1).unwrap().0;
                right_click(&mut app, area.x, area.y);
                assert!(app.context_menu_active);
                key(&mut app, KeyCode::Enter);
            }
            _ => {
                let area = app.tab_bar_area.unwrap();
                let col = (area.x..area.right())
                    .find(|&col| crate::mouse::find_tab_at_col(&app, col, area) == Some(1)).unwrap();
                right_click(&mut app, col, area.y);
                assert!(app.tab_context_active);
                app.tab_context_selected = App::tab_context_items().iter()
                    .position(|(_, command, _)| *command == "tab_rename_terminal").unwrap();
                key(&mut app, KeyCode::Enter);
            }
        }
        assert_eq!(app.terminal_rename.target, Some(1), "route={route}");
        assert_eq!(app.focused_text_input_id(), Some(TextInputId::TerminalRename));
        assert_eq!(app.terminal_rename.query, "Second");
        assert_eq!(app.terminal_rename.anchor, Some(0));
        assert!(app.paste_into_focused_text_input(" Build – café "));
        assert_eq!(app.terminals[1].name, "Second", "names change only on confirmation");
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.terminals[1].name, "Build – café");
        assert_eq!(app.terminals[0].name, "First");
        assert_eq!(app.tabs[0].editor.rope.to_string(), "unchanged document");
        assert_eq!(app.terminal_panel_focused, route != 3, "restore focus, route={route}");
        assert!(app.terminals.iter().all(|terminal| terminal.terminal_pty.is_none()));
        let terminal = draw(&mut app);
        let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(screen.contains("Build – café"), "updated tab label, route={route}");
    }
}

#[test]
fn terminal_rename_validation_and_cancel_preserve_names_and_focus() {
    let mut app = fixture();
    app.begin_terminal_rename(0);
    app.terminal_rename.query = "   ".into();
    key(&mut app, KeyCode::Enter);
    assert!(app.modal.error.is_some());
    assert_eq!(app.terminals[0].name, "First");
    app.terminal_rename.query = "name\u{1b}escape".into();
    key(&mut app, KeyCode::Enter);
    assert!(app.modal.error.is_some());
    assert_eq!(app.terminals[0].name, "First");
    app.terminal_rename.query = "Unconfirmed".into();
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.terminals[0].name, "First");
    assert!(app.terminal_rename.target.is_none());
    assert!(app.terminal_panel_focused);
}

#[test]
fn terminal_rename_targets_follow_earlier_closes_and_cancel_when_the_target_exits() {
    let mut app = fixture();
    app.begin_terminal_rename(2);
    app.terminal_rename.query = "编译".into();
    app.close_terminal_tab_at(0);
    assert_eq!(app.terminal_rename.target, Some(1));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.terminals[0].name, "Second");
    assert_eq!(app.terminals[1].name, "编译");
    app.begin_terminal_rename(1);
    app.close_terminal_tab_at(1);
    assert!(app.terminal_rename.target.is_none());
    assert!(crate::ui_modal::active(&app).is_none());
    assert_eq!(app.terminals[0].name, "Second");

    let mut app = fixture();
    app.open_terminal_context_menu(2, 0, 0);
    app.close_terminal_tab_at(0);
    app.rename_context_terminal();
    assert_eq!(app.terminal_rename.query, "Third");
    key(&mut app, KeyCode::Esc);
    app.open_terminal_context_menu(1, 0, 0);
    app.close_terminal_tab_at(1);
    app.rename_context_terminal();
    assert!(app.terminal_rename.target.is_none());
}

#[test]
fn custom_terminal_names_survive_moves_between_editor_and_dock() {
    let mut app = fixture();
    app.terminals[1].pending_command = Some("cargo test".into());
    app.begin_terminal_rename(1);
    app.terminal_rename.query = "Tests".into();
    key(&mut app, KeyCode::Enter);
    assert!(app.detach_terminal_to_group(1, app.active_group));
    assert!(draw(&mut app).backend().buffer().content.iter().map(|cell| cell.symbol()).collect::<String>().contains("Tests"));
    assert!(app.move_terminal_to_dock(1, TerminalDock::Right));
    assert_eq!(app.terminals[1].name, "Tests");
    assert_eq!(app.terminals[1].pending_command.as_deref(), Some("cargo test"));
    assert!(app.terminals[1].terminal_pty.is_none());
}

#[test]
fn terminal_rename_is_unavailable_for_empty_slots_and_file_tabs() {
    let mut app = App::test_new();
    assert!(app.is_menu_cmd_disabled("rename_terminal"));
    app.execute_palette_command("rename_terminal");
    assert!(app.terminal_rename.target.is_none());
    app.tabs.push(Tab::new_file(Editor::test_with_text("file")));
    app.tab_context_tab = 0;
    assert!(app.tab_context_command_disabled("tab_rename_terminal"));
}
