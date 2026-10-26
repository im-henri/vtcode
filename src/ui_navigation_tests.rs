use super::*;
use crate::app::{ActivePane, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn navigation_app() -> App {
    let mut app = App::test_new();
    app.explorer.root = PathBuf::from("vtcode_git");
    let mut editor = crate::editor::Editor::new();
    editor.rope = ropey::Rope::from_str(&"content\n".repeat(100));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.record_navigation();
    for line in [30, 60] {
        app.editor_mut().unwrap().set_cursor(line, 0);
        app.finish_navigation();
    }
    app
}

fn draw(app: &mut App, area: Rect) -> Buffer {
    app.menu_bar_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(area.right(), area.bottom() + 5)).unwrap();
    terminal
        .draw(|frame| {
            draw_menu_bar(frame, app, area);
            draw_navigation_tooltip(frame, app);
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn arrow(buffer: &Buffer, area: Rect, forward: bool) -> u16 {
    (area.x..area.right())
        .find(|&x| buffer[(x, area.y)].symbol() == if forward { "→" } else { "←" })
        .expect("visible navigation arrow")
}

fn mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn click(app: &mut App, col: u16, row: u16) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        mouse(app, kind, col, row);
    }
}

#[test]
fn title_buttons_share_history_with_keyboard_and_side_buttons() {
    let mut app = navigation_app();
    let area = Rect::new(0, 0, 140, 1);
    let buffer = draw(&mut app, area);
    crate::ui_control_visibility_tests::snapshot(&buffer, "Navigation - newest stop");
    let back = arrow(&buffer, area, false);
    let forward = arrow(&buffer, area, true);
    let disabled = buffer[(forward, 0)].fg;
    assert_ne!(buffer[(back, 0)].fg, disabled);
    click(&mut app, back, 0);
    assert_eq!(
        app.editor().unwrap().cursor_line,
        30,
        "one gesture navigates once"
    );
    assert!(app.navigation.can_navigate(false) && app.navigation.can_navigate(true));
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Left, KeyModifiers::ALT),
    );
    assert_eq!(app.editor().unwrap().cursor_line, 0);
    assert!(!app.navigation.can_navigate(false));
    assert_eq!(draw(&mut app, area)[(back, 0)].fg, disabled);
    click(&mut app, back, 0);
    assert_eq!(
        app.editor().unwrap().cursor_line,
        0,
        "disabled button is a no-op"
    );
    // The setting for physical side buttons must not disable the title bar.
    app.mouse_back_forward_to_navigate = false;
    click(&mut app, forward, 0);
    assert_eq!(app.editor().unwrap().cursor_line, 30);
    app.mouse_back_forward_to_navigate = true;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Forward), 0, 0);
    assert_eq!(app.editor().unwrap().cursor_line, 60);
    assert!(!app.navigation.can_navigate(true));
    assert!(!app.menu_active && !app.exit_confirm_active);
}

#[test]
fn navigation_arrows_stay_aligned_and_clear_of_other_controls_when_resized() {
    let mut app = navigation_app();
    for name in [
        "vtcode_git",
        "项目",
        "a workspace name that needs much more room",
    ] {
        app.explorer.root = PathBuf::from(name);
        for x in [0, 5] {
            let mut previously_visible = false;
            for width in 1..=160 {
                let area = Rect::new(x, 2, width, 1);
                let buffer = draw(&mut app, area);
                let visible = (area.x..area.right()).any(|col| buffer[(col, 2)].symbol() == "←");
                assert!(
                    !previously_visible || visible,
                    "arrows disappear as the window grows"
                );
                previously_visible = visible;
                if !visible {
                    assert!(
                        (area.x..area.right())
                            .all(|col| navigation_button_at(&app, col, 2).is_none())
                    );
                    continue;
                }
                for forward in [false, true] {
                    let col = arrow(&buffer, area, forward);
                    for button_col in col - 1..=col + 1 {
                        assert_eq!(
                            navigation_button_at(&app, button_col, 2)
                                .map(|(_, direction)| direction),
                            Some(forward)
                        );
                        assert_eq!(buffer[(button_col, 2)].bg, buffer[(col, 2)].bg);
                    }
                    assert!(col + 1 < agent_chat_button_area(area).unwrap().x);
                }
                assert_eq!(buffer[(area.right() - 2, 2)].symbol(), "×");
            }
        }
    }
    app.menu_bar_area = None;
    assert!(navigation_button_at(&app, 70, 0).is_none());
}

#[test]
fn navigation_tooltips_follow_shortcuts_and_hide_under_modals() {
    let mut app = navigation_app();
    let area = Rect::new(0, 0, 140, 1);
    let buffer = draw(&mut app, area);
    app.mouse_col = arrow(&buffer, area, false);
    app.mouse_row = 0;
    let hovered = draw(&mut app, area);
    crate::ui_control_visibility_tests::snapshot(&hovered, "Navigation - back hovered");
    assert!(text(&hovered).contains("Go Back (Alt+Left)"));
    let shortcut = app
        .keybindings
        .iter_mut()
        .find(|key| key.id == "navigate_back")
        .unwrap();
    shortcut.code = KeyCode::F(12);
    shortcut.modifiers = KeyModifiers::CONTROL;
    assert!(text(&draw(&mut app, area)).contains("Go Back (Ctrl+F12)"));
    app.keybindings
        .iter_mut()
        .find(|key| key.id == "navigate_back")
        .unwrap()
        .code = KeyCode::Null;
    let no_shortcut = text(&draw(&mut app, area));
    assert!(no_shortcut.contains("Go Back"));
    assert!(!no_shortcut.contains("Go Back ("));
    app.mouse_col = arrow(&buffer, area, true);
    assert!(text(&draw(&mut app, area)).contains("Go Forward (Alt+Right)"));
    app.exit_confirm_active = true;
    assert!(!text(&draw(&mut app, area)).contains("Go Forward"));
    let back = arrow(&buffer, area, false);
    click(&mut app, back, 0);
    assert_eq!(
        app.editor().unwrap().cursor_line,
        60,
        "modal consumes navigation clicks"
    );
}

#[test]
fn hovering_compact_navigation_does_not_switch_an_open_menu() {
    let mut app = navigation_app();
    let area = Rect::new(0, 0, 40, 1);
    let buffer = draw(&mut app, area);
    let back = arrow(&buffer, area, false);
    app.menu_active = true;
    app.menu_open = true;
    app.menu_selected = 0;
    mouse(&mut app, MouseEventKind::Moved, back, 0);
    assert_eq!(app.menu_selected, 0);
    assert!(!text(&draw(&mut app, area)).contains("Go Back"));
    click(&mut app, back, 0);
    assert_eq!(app.editor().unwrap().cursor_line, 30);
    assert!(!app.menu_active && !app.menu_open);
}

#[test]
fn leaving_navigation_for_editor_clears_hover_and_tooltip() {
    let mut app = navigation_app();
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let area = app.menu_bar_area.unwrap();
    let back = arrow(terminal.backend().buffer(), area, false);
    let code = app.code_area.unwrap();
    mouse(&mut app, MouseEventKind::Moved, code.x + 2, code.y + 2);
    let editor_hover = app.hovered_editor_pos;
    mouse(&mut app, MouseEventKind::Moved, back, area.y);
    app.hovered_editor_pos = editor_hover;
    app.mouse_hover_changed = false;
    mouse(&mut app, MouseEventKind::Moved, code.x + 2, code.y + 2);
    assert!(app.mouse_hover_changed);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(!text(terminal.backend().buffer()).contains("Go Back ("));
}
