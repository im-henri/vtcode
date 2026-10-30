use super::*;
use crate::app::{SidebarView, Tab};
use crate::editor::Editor;
use crate::text_input::TextInputId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        "unchanged document\n",
    ))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_minimap = false;
    app.mouse_col = 80;
    app.mouse_row = 15;
    app
}

fn draw(app: &mut App) {
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn hover(app: &mut App, column: u16, row: u16) {
    crate::mouse::handle_mouse(app, mouse(MouseEventKind::Moved, column, row));
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn click(app: &mut App, column: u16, row: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(app, mouse(kind, column, row));
    }
}

#[test]
fn sidebar_auto_hide_is_off_by_default_and_disabled_hover_preserves_layout() {
    let mut app = fixture();
    assert!(!app.sidebar_auto_hide.enabled);
    assert_eq!(app.current_settings().sidebar_auto_hide, Some(false));
    draw(&mut app);
    let sidebar = app
        .explorer_area
        .expect("sidebar remains visible by default");
    let editor = app.code_area.unwrap();
    hover(&mut app, 0, sidebar.y + 2);
    hover(&mut app, editor.right() - 2, editor.y + 2);
    draw(&mut app);
    assert_eq!(app.explorer_area, Some(sidebar));
    assert_eq!(app.code_area, Some(editor));
}

#[test]
fn sidebar_hover_reveals_every_view_and_restores_editor_space_without_taking_focus() {
    for view in [
        SidebarView::Explorer,
        SidebarView::Search,
        SidebarView::SourceControl,
        SidebarView::Settings,
        SidebarView::Extensions,
        SidebarView::Outline,
    ] {
        for right in [false, true] {
            for activity_bar in [false, true] {
                let mut app = fixture();
                app.sidebar_view = view;
                app.global_search_active = view == SidebarView::Search;
                app.sidebar_right = right;
                app.show_activity_bar = activity_bar;
                app.set_sidebar_auto_hide(true);
                draw(&mut app);
                assert!(app.explorer_area.is_none(), "initially hidden, {view:?}");
                let editor = app.code_area.unwrap();
                let edge = app.sidebar_auto_hide.edge.unwrap();
                hover(&mut app, edge.x, edge.y + 2);
                assert!(app.sidebar_is_visible(), "reveal {view:?}");
                assert_eq!(app.active_pane, ActivePane::Editor);
                draw(&mut app);
                let sidebar = app.explorer_area.expect("hover reveals sidebar");
                assert!(app.code_area.unwrap().width < editor.width);
                hover(&mut app, sidebar.x + sidebar.width / 2, sidebar.y + 2);
                assert!(app.sidebar_is_visible(), "keep open while entering it");
                let code = app.code_area.unwrap();
                hover(&mut app, code.x + code.width / 2, code.y + 2);
                draw(&mut app);
                assert!(app.explorer_area.is_none(), "hide after leaving, {view:?}");
                assert_eq!(app.code_area.unwrap().width, editor.width);
                assert!(
                    app.show_explorer,
                    "hover does not change the visibility preference"
                );
                assert!(app.build_session().show_explorer);
                assert_eq!(app.tabs[0].editor.rope.to_string(), "unchanged document\n");
                for field in [
                    TextInputId::GlobalSearch(0),
                    TextInputId::GlobalSearch(1),
                    TextInputId::GlobalSearch(2),
                    TextInputId::GlobalSearch(3),
                    TextInputId::ExtensionsFilter,
                ] {
                    assert!(app.text_input_area(field).is_none());
                }
            }
        }
    }
}

#[test]
fn activity_bar_reveals_sidebar_and_menu_and_status_edges_do_not() {
    let mut app = fixture();
    app.set_sidebar_auto_hide(true);
    draw(&mut app);
    let activity = app.activity_bar_area.unwrap();
    hover(&mut app, activity.x + 2, activity.y + 2);
    draw(&mut app);
    assert!(app.explorer_area.is_some());
    let code = app.code_area.unwrap();
    hover(&mut app, code.right() - 2, code.y + 2);
    draw(&mut app);
    for row in [app.menu_bar_area.unwrap().y, app.status_bar_area.unwrap().y] {
        hover(&mut app, 0, row);
        assert!(!app.sidebar_is_visible());
    }
    assert_eq!(app.active_pane, ActivePane::Editor);
}

#[test]
fn sidebar_search_stays_open_while_typing_and_hides_after_editor_focus_returns() {
    let mut app = fixture();
    app.set_sidebar_auto_hide(true);
    app.focus_global_search();
    draw(&mut app);
    let code = app.code_area.unwrap();
    hover(&mut app, code.x + 10, code.y + 2);
    draw(&mut app);
    assert!(app.explorer_area.is_some());
    key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "x");
    assert_eq!(app.tabs[0].editor.rope.to_string(), "unchanged document\n");
    click(&mut app, code.x + 10, code.y + 2);
    draw(&mut app);
    assert!(app.explorer_area.is_none());
    assert!(app.focused_text_input_id().is_none());
    key(&mut app, KeyCode::Char('y'), KeyModifiers::NONE);
    assert_eq!(app.global_search_query, "x");
    assert!(app.tabs[0].editor.rope.to_string().contains('y'));
    key(
        &mut app,
        KeyCode::Char('F'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    draw(&mut app);
    assert!(
        app.explorer_area.is_some(),
        "keyboard shortcut reveals Search again"
    );
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::GlobalSearch(0))
    );
}

#[test]
fn sidebar_context_menu_and_resize_keep_the_sidebar_stable() {
    let mut app = fixture();
    app.set_sidebar_auto_hide(true);
    draw(&mut app);
    let edge = app.sidebar_auto_hide.edge.unwrap();
    hover(&mut app, edge.x, edge.y + 2);
    draw(&mut app);
    let sidebar = app.explorer_area.unwrap();
    let code = app.code_area.unwrap();
    app.open_explorer_workspace_context_menu(sidebar.x + 2, sidebar.y + 2);
    hover(&mut app, code.right() - 2, code.y + 2);
    draw(&mut app);
    assert_eq!(app.explorer_area, Some(sidebar));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.explorer_context_active);
    app.resizing_explorer = true;
    hover(&mut app, code.right() - 2, code.y + 2);
    assert!(app.sidebar_is_visible());
    app.resizing_explorer = false;
    hover(&mut app, code.right() - 2, code.y + 2);
    draw(&mut app);
    assert!(app.explorer_area.is_none());
    app.open_palette();
    hover(&mut app, edge.x, edge.y + 2);
    assert!(!app.sidebar_is_visible(), "do not reveal behind a popup");
}

#[test]
fn hidden_source_control_does_not_keep_repository_hit_regions_over_the_editor() {
    let mut app = fixture();
    app.sidebar_view = SidebarView::SourceControl;
    app.set_sidebar_auto_hide(true);
    app.reveal_sidebar();
    draw(&mut app);
    let sidebar = app.explorer_area.unwrap();
    let code = app.code_area.unwrap();
    // Repository headers keep a separate detail rectangle while rendered.
    app.repositories.detail_area = Some(sidebar);
    hover(&mut app, code.right() - 2, code.y + 2);
    draw(&mut app);
    assert!(app.explorer_area.is_none());
    assert!(app.source_control_detail_area().is_none());
    crate::mouse::handle_mouse(
        &mut app,
        mouse(
            MouseEventKind::Down(MouseButton::Right),
            sidebar.x + 10,
            sidebar.y + 8,
        ),
    );
    assert!(!app.sc_context_active);
    assert!(
        app.context_menu_active,
        "the editor owns the context menu after auto-hide"
    );
}

#[test]
fn sidebar_toggle_and_keyboard_focus_work_while_auto_hidden_and_manual_hiding_is_respected() {
    let mut app = fixture();
    app.set_sidebar_auto_hide(true);
    draw(&mut app);
    let edge = app.sidebar_auto_hide.edge.unwrap();
    key(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    draw(&mut app);
    assert!(
        app.explorer_area.is_some(),
        "Ctrl+B reveals an auto-hidden sidebar"
    );
    key(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
    hover(&mut app, edge.x, edge.y + 2);
    draw(&mut app);
    assert!(
        app.explorer_area.is_none(),
        "explicitly hidden stays hidden"
    );
    key(
        &mut app,
        KeyCode::Char('E'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    draw(&mut app);
    assert!(app.explorer_area.is_some());
    assert_eq!(app.active_pane, ActivePane::Explorer);
    app.set_sidebar_auto_hide(false);
    app.active_pane = ActivePane::Editor;
    let code = app.code_area.unwrap();
    hover(&mut app, code.right() - 2, code.y + 2);
    draw(&mut app);
    assert!(
        app.explorer_area.is_some(),
        "turning the option off restores normal visibility"
    );
}

#[test]
fn sidebar_auto_hide_opens_from_palette_and_the_view_menu_and_is_a_workbench_checkbox() {
    let mut app = fixture();
    assert_eq!(App::setting_field_kind("sidebar_auto_hide"), "checkbox");
    assert_eq!(App::settings_category_for_id("sidebar_auto_hide"), 2);
    assert_eq!(
        App::settings_section_for_id("sidebar_auto_hide"),
        Some("Layout")
    );
    for query in ["autohide", "auto hide", "sidebarAutoHide"] {
        app.settings_editor_search = query.into();
        let items = app.settings_items();
        assert!(
            app.settings_filtered_indices_with_items(0, &items)
                .iter()
                .any(|&index| items[index].2 == "sidebar_auto_hide"),
            "query={query}"
        );
    }
    app.open_palette();
    assert!(app.paste_into_focused_text_input("View: Toggle Sidebar Auto-hide"));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.palette.active);
    assert!(app.sidebar_auto_hide.enabled);
    app.menu_active = true;
    app.menu_open = true;
    app.menu_selected = 3;
    let items = app.menu_items(3);
    app.menu_item_selected = items
        .iter()
        .position(|(_, id)| *id == "toggle_sidebar_auto_hide")
        .unwrap();
    assert!(items[app.menu_item_selected].0.starts_with('✓'));
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.sidebar_auto_hide.enabled);
    assert!(!app.menu_active && !app.menu_open);
}

#[test]
fn sidebar_hover_layout_changes_request_a_redraw_even_for_repeated_pointer_coordinates() {
    let mut app = fixture();
    app.set_sidebar_auto_hide(true);
    draw(&mut app);
    let edge = app.sidebar_auto_hide.edge.unwrap();
    let point = (edge.x, edge.y + 2);
    app.mouse_col = point.0;
    app.mouse_row = point.1;
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
    assert!(crate::app_event_loop::handle_mouse_input(
        &mut app,
        &mut pump,
        mouse(MouseEventKind::Moved, point.0, point.1)
    ));
    draw(&mut app);
    assert!(app.explorer_area.is_some());
    let code = app.code_area.unwrap();
    app.mouse_col = code.right() - 2;
    app.mouse_row = code.y + 2;
    let event = mouse(MouseEventKind::Moved, app.mouse_col, app.mouse_row);
    assert!(crate::app_event_loop::handle_mouse_input(
        &mut app, &mut pump, event
    ));
    draw(&mut app);
    assert!(app.explorer_area.is_none());
}

#[test]
fn sidebar_auto_hide_respects_zen_and_small_viewports() {
    for (width, height) in [(40, 12), (16, 5), (3, 3), (1, 1)] {
        let mut app = fixture();
        app.set_sidebar_auto_hide(true);
        for zen in [false, true] {
            app.zen_mode = zen;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            if let Some(edge) = app.sidebar_auto_hide.edge {
                hover(&mut app, edge.x, edge.y);
            }
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            if zen {
                assert!(app.explorer_area.is_none());
            }
        }
    }
}
