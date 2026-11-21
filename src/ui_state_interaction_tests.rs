use crate::{
    app::{ActivePane, App, Tab, TabKind},
    editor::Editor,
    explorer::{FileEntry, GitStatus},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

#[path = "ui_overlay_combination_tests.rs"]
mod overlays;

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
    app.rebuild_keybinding_settings_cache();
    app.execute_palette_command("keybindings");
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

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect()
        })
        .collect()
}

/// Screen position of the first cell of `text` on a rendered line.
fn find(terminal: &Terminal<TestBackend>, text: &str) -> (u16, u16) {
    screen(terminal)
        .iter()
        .enumerate()
        .find_map(|(y, line)| {
            line.find(text)
                .map(|offset| (line[..offset].chars().count() as u16, y as u16))
        })
        .unwrap_or_else(|| panic!("{text:?} not rendered"))
}

fn selected_binding(app: &App) -> Option<usize> {
    crate::ui_settings_shortcuts::binding_index(&app.settings_items()[app.settings_selected].2)
}

#[test]
fn keyboard_shortcuts_command_opens_settings_page_instead_of_a_tab() {
    let mut app = fixture();
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[app.active_tab].kind, TabKind::Settings);
    assert_eq!(
        app.settings_category_selected,
        crate::ui_settings_shortcuts::CATEGORY
    );
    assert!(selected_binding(&app).is_some());
    // Running it again reuses the same Settings tab.
    app.execute_palette_command("keybindings");
    assert_eq!(app.tabs.len(), 2);
}

#[test]
fn opening_shortcuts_releases_docked_terminal_focus() {
    let mut app = fixture();
    app.terminal_panel_focused = true;
    app.active_pane = ActivePane::Panel;
    app.execute_palette_command("keybindings");
    assert_eq!(app.active_pane, ActivePane::Editor);
    assert!(!app.terminal_panel_focused);
}

#[test]
fn shortcut_table_uses_one_line_per_command_with_a_column_header() {
    let mut app = fixture();
    let terminal = draw(&mut app);
    let (_, header) = find(&terminal, "Command");
    let lines = screen(&terminal);
    assert!(lines[header as usize].contains("Shortcut") && lines[header as usize].contains("When"));
    let first = app
        .keybinding_settings_cache
        .iter()
        .filter(|(_, _, id)| id.starts_with("kb_"))
        .take(2)
        .map(|(label, _, _)| label.trim_start_matches("Shortcut: ").to_owned())
        .collect::<Vec<_>>();
    let (_, a) = find(&terminal, &first[0]);
    let (_, b) = find(&terminal, &first[1]);
    assert_eq!(b, a + 1, "consecutive commands sit on consecutive lines");
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("[Unassign]") || line.contains("Shortcut: "))
    );
}

#[test]
fn single_click_selects_and_double_click_or_key_cap_edits() {
    let mut app = fixture();
    let terminal = draw(&mut app);
    let next_label = app
        .settings_items()
        .iter()
        .filter_map(|(label, _, id)| id.starts_with("kb_").then_some(label.clone()))
        .nth(1)
        .unwrap();
    let (x, next_y) = find(&terminal, next_label.trim_start_matches("Shortcut: "));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        x + 1,
        next_y,
    );
    assert!(
        app.shortcut_editor.is_none(),
        "one click on the name only selects"
    );
    assert_ne!(selected_binding(&app), None);
    assert!(app.settings_focus_control);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        x + 1,
        next_y,
    );
    assert!(app.shortcut_editor.is_some(), "double-click edits");
    key(&mut app, KeyCode::Esc);
    assert!(app.shortcut_editor.is_none());
    assert_eq!(
        app.tabs.len(),
        2,
        "Escape cancels capture before closing Settings"
    );

    // The key cap is itself a button.
    let terminal = draw(&mut app);
    let keys = app.keybindings[selected_binding(&app).unwrap()].display();
    let (kx, ky) = find(&terminal, &format!(" {keys} "));
    assert_eq!(ky, next_y);
    app.settings_shortcut_click = None;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        kx + 1,
        ky,
    );
    assert!(app.shortcut_editor.is_some());
}

#[test]
fn hovered_row_offers_change_and_remove_buttons() {
    let mut app = fixture();
    let terminal = draw(&mut app);
    let index = selected_binding(&app).unwrap();
    let (x, y) = find(&terminal, app.keybindings[index].label);
    mouse(&mut app, MouseEventKind::Moved, x, y);
    let terminal = draw(&mut app);
    let line = &screen(&terminal)[y as usize];
    assert!(line.contains(" Change "), "{line}");
    assert!(line.contains(" Remove "), "{line}");
    let (rx, ry) = find(&terminal, " Remove ");
    assert_eq!(ry, y);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        rx + 2,
        ry,
    );
    assert_eq!(app.keybindings[index].display(), "Unassigned");
}

#[test]
fn delete_key_removes_the_focused_shortcut_but_not_search_text() {
    let mut app = fixture();
    draw(&mut app);
    key(&mut app, KeyCode::Down);
    let index = selected_binding(&app).unwrap();
    assert_ne!(app.keybindings[index].display(), "Unassigned");
    key(&mut app, KeyCode::Delete);
    assert_eq!(app.keybindings[index].display(), "Unassigned");

    app.settings_focus_control = false;
    app.settings_editor_search = "ctrl".into();
    app.settings_editor_search_cursor = 4;
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.settings_editor_search, "ctr");
}

#[test]
fn enter_on_a_row_opens_the_shortcut_editor() {
    let mut app = fixture();
    draw(&mut app);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Enter);
    assert!(app.shortcut_editor.is_some());
    assert!(app.settings_kb_editing);
}

#[test]
fn search_by_key_finds_shortcut_rows() {
    let mut app = fixture();
    app.settings_editor_search = "ctrl+shift+p".into();
    let items = app.settings_items();
    let found = app.settings_filtered_indices_with_items(app.settings_category_selected, &items);
    assert!(!found.is_empty());
    assert!(found.iter().all(|&index| items[index].2.starts_with("kb_")));
}

#[test]
fn settings_shortcut_page_does_not_steal_explorer_wheel_or_keys() {
    let mut app = fixture();
    draw(&mut app);
    let tree = crate::ui_chrome::explorer_tree_content_area(&app, app.explorer_area.unwrap());
    let selected = app.settings_selected;
    mouse(&mut app, MouseEventKind::ScrollDown, tree.x + 2, tree.y + 2);
    assert!(app.explorer.scroll_offset > 0);
    assert_eq!(app.settings_selected, selected);
    app.active_pane = ActivePane::Explorer;
    app.explorer.selected = 0;
    key(&mut app, KeyCode::Down);
    assert_eq!(app.explorer.selected, 1);
    assert_eq!(app.settings_selected, selected);
}

#[test]
fn shortcut_assignment_cannot_capture_a_popup_dismissal() {
    // A code-action flag without actions is not a visible popup, so it is not covered.
    for popup in [0, 1, 2, 4, 5] {
        let mut app = fixture();
        draw(&mut app);
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        assert!(app.shortcut_editor.is_some());
        match popup {
            0 => app.shortcuts_active = true,
            1 => app.call_hierarchy_active = true,
            2 => app.type_hierarchy_active = true,
            4 => app.context_menu_active = true,
            _ => app.quick_open_active = true,
        }
        key(&mut app, KeyCode::Esc);
        assert!(
            app.shortcut_editor.is_some(),
            "the popup owns Escape; popup={popup}"
        );
        assert_eq!(app.tabs.len(), 2, "popup={popup}");
    }
}
