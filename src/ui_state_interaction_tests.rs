use crate::{
    app::{ActivePane, App, Tab},
    editor::Editor,
    explorer::{FileEntry, GitStatus},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> App {
    let mut app = App::test_new();
    app.source_control_enabled = false;
    app.show_explorer = true;
    app.show_panel = false;
    app.show_minimap = false;
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        &"file text\n".repeat(200),
    ))];
    app.explorer.entries = (0..100)
        .map(|index| FileEntry {
            path: app.explorer.root.join(format!("file-{index}.rs")),
            name: format!("file-{index}.rs"),
            is_dir: false,
            depth: 0,
            expanded: false,
            git_status: GitStatus::Unmodified,
            git_status_summary: Default::default(),
        })
        .collect();
    app.open_keybindings_tab();
    app
}

fn draw(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(180, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal
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

#[test]
fn ui_combinations_shortcuts_tab_does_not_swallow_file_tab_clicks() {
    let mut app = fixture();
    draw(&mut app);
    let tabs = app.tab_bar_area.unwrap();
    let file = crate::ui_chrome_tabs::TabBarHits::new(&app, tabs).tabs[0].1;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        file.x + 2,
        file.y,
    );
    assert_eq!(app.active_tab, 0);
}

#[test]
fn ui_combinations_shortcuts_tab_does_not_steal_explorer_wheel() {
    let mut app = fixture();
    draw(&mut app);
    let tree = crate::ui_chrome::explorer_tree_content_area(&app, app.explorer_area.unwrap());
    let selected = app.keybindings_selected;
    mouse(&mut app, MouseEventKind::ScrollDown, tree.x + 2, tree.y + 2);
    assert!(app.explorer.scroll_offset > 0);
    assert_eq!(app.keybindings_selected, selected);
}

#[test]
fn ui_combinations_shortcuts_tab_does_not_steal_sidebar_keys() {
    let mut app = fixture();
    draw(&mut app);
    app.active_pane = ActivePane::Explorer;
    app.explorer.selected = 0;
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    assert_eq!(app.explorer.selected, 1);
    assert_eq!(app.keybindings_selected, 0);
}

#[test]
fn ui_combinations_shortcuts_tab_preserves_quick_open_typing_and_tab_switching() {
    let mut app = fixture();
    draw(&mut app);
    app.quick_open_active = true;
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
    );
    assert_eq!(app.quick_open_query, "n");
    app.quick_open_active = false;
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL),
    );
    assert!(app.tab_switcher_active);
}

#[test]
fn ui_combinations_shortcut_rows_follow_rendered_geometry_and_scroll() {
    let mut app = fixture();
    app.keybindings_selected = app.keybindings.len() - 1;
    draw(&mut app);
    let area = app.editor_area.unwrap();
    let visible = area.height.saturating_sub(4) as usize;
    let first = app
        .keybindings_selected
        .saturating_sub(visible.saturating_sub(1));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.right() - 2,
        area.y + 4,
    );
    assert_eq!(app.keybindings_selected, first + 1);
    assert!(app.keybindings_editing);
}

#[test]
fn ui_combinations_shortcuts_tab_allows_status_customization() {
    let mut app = fixture();
    draw(&mut app);
    let bar = app.status_bar_area.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Right),
        bar.x + 2,
        bar.y,
    );
    assert!(app.status_details.open);
    assert!(app.status_details.customize);
}

#[test]
fn ui_combinations_shortcut_capture_waits_while_another_control_has_focus() {
    let mut app = fixture();
    draw(&mut app);
    app.keybindings_editing = true;
    app.active_pane = ActivePane::Explorer;
    let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    crate::input_keyboard::handle_keyboard_event(&mut app, escape);
    assert!(app.keybindings_editing);
    app.active_pane = ActivePane::Editor;
    app.quick_open_active = true;
    crate::input_keyboard::handle_keyboard_event(&mut app, escape);
    assert!(!app.quick_open_active);
    assert!(app.keybindings_editing);
    crate::input_keyboard::handle_keyboard_event(&mut app, escape);
    assert!(!app.keybindings_editing);
    assert_eq!(
        app.tabs.len(),
        2,
        "Escape cancels assignment before closing the tab"
    );
    crate::input_keyboard::handle_keyboard_event(&mut app, escape);
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn ui_combinations_shortcuts_ignore_stale_hit_areas_and_actual_modals_block_input() {
    let mut app = fixture();
    draw(&mut app);
    let area = app.editor_area.unwrap();
    app.active_tab = 0;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 2,
        area.y + 3,
    );
    assert!(!app.keybindings_editing);
    assert_eq!(app.active_tab, 0);

    app.open_keybindings_tab();
    app.confirm_close_active = true;
    draw(&mut app);
    let selected = app.keybindings_selected;
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    assert_eq!(app.keybindings_selected, selected);
    assert!(!app.keybindings_editing);
    assert!(app.confirm_close_active);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 2,
        area.y + 3,
    );
    assert!(
        !app.keybindings_editing,
        "dismissing the dialog cannot click the table underneath"
    );
    assert_eq!(app.keybindings_selected, selected);
    assert!(!app.confirm_close_active);
}

#[test]
fn ui_combinations_shortcuts_keep_keyboard_navigation_and_explicit_edit_cancellation() {
    let mut app = fixture();
    draw(&mut app);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    assert_eq!(app.keybindings_selected, 1);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    );
    assert_eq!(app.keybindings_selected, 0);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(app.keybindings_editing);
    let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    crate::input_keyboard::handle_keyboard_event(&mut app, escape);
    assert!(!app.keybindings_editing);
    assert_eq!(app.tabs.len(), 2);
    crate::input_keyboard::handle_keyboard_event(&mut app, escape);
    assert_eq!(app.tabs.len(), 1);
}

#[test]
fn ui_combinations_inactive_shortcut_rows_own_their_drag_without_selecting_source() {
    let mut app = fixture();
    app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
    app.switch_group(0);
    let terminal = draw(&mut app);
    let code = app.code_area.unwrap();
    let buffer = terminal.backend().buffer();
    let label = app.keybindings[0].label;
    let (x, y) = (0..buffer.area.height)
        .find_map(|row| {
            let line: String = (0..buffer.area.width)
                .map(|col| buffer[(col, row)].symbol())
                .collect();
            line.find(label)
                .map(|offset| (line[..offset].chars().count() as u16, row))
        })
        .expect("a rendered shortcut row in the inactive pane");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    assert_eq!(app.active_group, 1);
    assert!(app.keybindings_editing);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        code.x + 5,
        code.y + 3,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        code.x + 5,
        code.y + 3,
    );
    assert!(app.groups[0].tabs[0].editor.selection_anchor.is_none());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        code.x + 5,
        code.y + 3,
    );
    assert_eq!(
        app.active_group, 0,
        "the released gesture must not swallow a later source click"
    );
}

#[test]
fn ui_combinations_opening_shortcuts_releases_docked_terminal_focus() {
    let mut app = fixture();
    app.terminal_panel_focused = true;
    app.active_pane = ActivePane::Panel;
    app.open_keybindings_tab();
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert!(!app.terminal_panel_focused);
}

#[test]
fn ui_combinations_shortcut_assignment_cannot_capture_a_popup_dismissal() {
    for popup in 0..7 {
        let mut app = fixture();
        draw(&mut app);
        app.keybindings_editing = true;
        match popup {
            0 => app.shortcuts_active = true,
            1 => app.call_hierarchy_active = true,
            2 => app.type_hierarchy_active = true,
            3 => app.lsp_code_action_active = true,
            4 => app.context_menu_active = true,
            5 => app.quick_open_active = true,
            _ => app.tab_switcher_active = true,
        }
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(
            app.keybindings_editing,
            "the popup owns Escape; popup={popup}"
        );
        assert_eq!(app.tabs.len(), 2);
    }
}
