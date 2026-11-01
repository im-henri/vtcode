use crate::app::{ActivePane, App, SidebarView, Tab};
use crate::editor::Editor;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn app(menu: usize) -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("word word\nsecond line\n"))];
    app.active_pane = ActivePane::Editor;
    app.menu_active = true;
    app.menu_open = true;
    app.menu_selected = menu;
    app
}

fn draw(app: &mut App, width: u16, height: u16) {
    app.menu_bar_area = Some(Rect::new(0, 0, width, 1));
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui_chrome::draw_menu_dropdown(frame, app)).unwrap();
    if width == 80 && height == 24 {
        crate::theme_visibility_tests::snapshot(&terminal, &format!("menu-{}", app.menu_selected));
    }
}

fn pointer(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    crate::mouse::handle_mouse(app, MouseEvent {
        kind, column, row, modifiers: KeyModifiers::NONE,
    });
}

#[test]
fn compact_menus_keep_selection_visible_and_hover_uses_rendered_rows() {
    for width in [28, 40, 80] {
        for height in [6, 12, 24] {
            for menu in 0..App::menu_labels().len() {
                let mut app = app(menu);
                let last = app.menu_items(menu).len() - 1;
                app.menu_item_selected = last;
                draw(&mut app, width, height);
                let inner = app.menu_dropdown_inner.unwrap();
                assert!(inner.right() <= width && inner.bottom() <= height);
                let row = inner.y + (last - app.menu_dropdown_scroll) as u16;
                assert!(row < inner.bottom(), "menu={menu}, size={width}x{height}");
                assert_eq!(app.menu_item_at(inner.x, row), Some(last));
                app.menu_item_selected = 0;
                pointer(&mut app, MouseEventKind::Moved, inner.x, row);
                assert_eq!(app.menu_item_selected, last);
            }
        }
    }
}

#[test]
fn end_key_and_mouse_activate_the_scrolled_help_action_after_left_clamping() {
    let mut app = app(6);
    crate::input_dialogs::handle_menu_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    let selected = app.menu_item_selected;
    assert_eq!(app.menu_items(6)[selected].1, "about");
    draw(&mut app, 40, 6);
    let inner = app.menu_dropdown_inner.unwrap();
    assert!(inner.x < crate::ui_chrome::menu_dropdown_anchor_x(&app));
    let row = inner.y + (selected - app.menu_dropdown_scroll) as u16;
    pointer(&mut app, MouseEventKind::Down(MouseButton::Left), inner.x, row);
    assert_eq!(crate::ui_modal::active(&app), Some(crate::ui_modal::Kind::About));
    assert!(!app.menu_active && !app.menu_open);
}

#[test]
fn wheel_reaches_the_end_of_view_without_scrolling_the_document() {
    let mut app = app(3);
    let editor_scroll = app.editor().unwrap().scroll_offset;
    for _ in 0..app.menu_items(3).len() {
        draw(&mut app, 50, 10);
        let inner = app.menu_dropdown_inner.unwrap();
        pointer(&mut app, MouseEventKind::ScrollDown, inner.x, inner.y);
    }
    draw(&mut app, 50, 10);
    let selected = app.menu_item_selected;
    assert_eq!(app.menu_items(3)[selected].1, "toggle_zen");
    assert!(app.menu_dropdown_scroll > 0);
    assert_eq!(app.editor().unwrap().scroll_offset, editor_scroll);
    let inner = app.menu_dropdown_inner.unwrap();
    let row = inner.y + (selected - app.menu_dropdown_scroll) as u16;
    pointer(&mut app, MouseEventKind::Down(MouseButton::Left), inner.x, row);
    assert!(app.zen_mode);
}

#[test]
fn explorer_action_selects_the_file_view_and_returns_keyboard_focus() {
    let mut app = app(3);
    app.menu_active = false;
    app.menu_open = false;
    app.sidebar_view = SidebarView::Search;
    app.global_search_active = true;
    app.terminal_panel_focused = true;
    app.execute_palette_command("show_explorer");
    assert_eq!(app.sidebar_view, SidebarView::Explorer);
    assert_eq!(app.active_pane, ActivePane::Explorer);
    assert!(app.sidebar_is_visible());
    assert!(!app.global_search_active && !app.terminal_panel_focused);
}

#[test]
fn clicking_a_menu_scrollbar_does_not_activate_a_command_behind_it() {
    let mut app = app(3);
    draw(&mut app, 50, 10);
    let inner = app.menu_dropdown_inner.unwrap();
    let text = app.editor().unwrap().rope.to_string();
    pointer(&mut app, MouseEventKind::Down(MouseButton::Left), inner.right() - 1, inner.y);
    assert!(app.menu_active && app.menu_open);
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    assert!(!app.palette.active);
}

#[test]
fn save_all_remains_available_when_settings_cover_a_modified_document() {
    let mut app = app(0);
    app.editor_mut().unwrap().insert_char('x');
    app.open_settings_tab();
    assert!(!app.is_menu_cmd_disabled("save_all"));
    assert!(app.is_menu_cmd_disabled("format_document"));
    assert!(app.is_menu_cmd_disabled("goto_symbol"));
}

#[test]
fn menus_reflect_reassigned_and_unassigned_shortcuts_without_default_fallbacks() {
    let mut app = app(3);
    let binding = app.keybindings.iter_mut().find(|key| key.id == "toggle_explorer").unwrap();
    binding.code = KeyCode::Char('b');
    binding.modifiers = KeyModifiers::ALT;
    let sidebar = app.menu_items(3).into_iter().find(|(_, id)| *id == "toggle_sidebar").unwrap().0;
    assert!(sidebar.ends_with("Alt+B"));
    for key in app.keybindings.iter_mut().filter(|key| key.id == "toggle_terminal") {
        key.code = KeyCode::Null;
    }
    let terminal = app.menu_items(5).into_iter().find(|(_, id)| *id == "toggle_terminal").unwrap().0;
    assert_eq!(terminal, "Show Right Terminal");
    app.refresh_shortcut_hints();
    assert!(app.palette.commands.iter().find(|command| command.id == "toggle_terminal").unwrap().shortcut.is_empty());
    assert_eq!(app.palette.commands.iter().find(|command| command.id == "goto_symbol").unwrap().shortcut, "Ctrl+Shift+O");
}

#[test]
fn replace_in_files_opens_the_fields_and_preserves_input_without_replacing() {
    let mut app = app(1);
    app.global_search_replace_visible = false;
    app.global_search_query = "word".into();
    app.global_search_replace_query = "updated".into();
    let text = app.editor().unwrap().rope.to_string();
    let command = app.menu_entries(1).into_iter().find(|entry| entry.label == "Replace in Files…").unwrap().command;
    app.execute_palette_command(command);
    assert_eq!(app.sidebar_view, SidebarView::Search);
    assert!(app.global_search_active && app.global_search_replace_visible);
    assert_eq!(app.search_focus_field, 1);
    assert_eq!(app.global_search_query, "word");
    assert_eq!(app.global_search_replace_query, "updated");
    assert!(!app.bg_global_replace_running);
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
}

#[test]
fn occurrence_and_duplicate_shortcuts_match_their_actual_editor_actions() {
    let mut app = app(2);
    app.menu_active = false;
    app.menu_open = false;
    let original = app.editor().unwrap().rope.to_string();
    crate::input_keyboard::handle_keyboard_event(&mut app,
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(app.editor().unwrap().rope.to_string(), original);
    assert!(app.editor().unwrap().has_selection());
    let items = app.menu_items(2);
    assert!(items.iter().find(|(_, id)| *id == "select_next_occurrence").unwrap().0.ends_with("Ctrl+D"));
    assert!(items.iter().find(|(_, id)| *id == "duplicate_line").unwrap().0.ends_with("Alt+Shift+Down"));
    app.editor_mut().unwrap().clear_selection();
    let lines = app.editor().unwrap().line_count();
    crate::input_keyboard::handle_keyboard_event(&mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::ALT | KeyModifiers::SHIFT));
    assert!(app.editor().unwrap().line_count() > lines);
}
