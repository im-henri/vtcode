use super::*;
use crate::{
    app::{ActivePane, Tab},
    app_theme::EditorTheme,
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};

const ACTIONS: &[Action] = &[Action {
    label: "About",
    command: "about",
}];

fn notice(message: &str) -> Notice {
    Notice {
        title: "Background service",
        message: message.into(),
        level: NotifyLevel::Warning,
        actions: ACTIONS,
    }
}

fn app() -> App {
    let mut app = App::test_new();
    app.source_control_enabled = false;
    app.show_explorer = false;
    app.show_minimap = false;
    app.active_pane = ActivePane::Editor;
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        &"sample text for selection\n".repeat(100),
    ))];
    app.side_notice
        .show("fixture", notice("Waiting for the service…"));
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn keyed_updates_preserve_dismissal_until_an_explicit_show_or_new_incident() {
    let mut state = State::default();
    assert!(!state.visible());
    state.publish("service", notice("Attempt 1"));
    assert!(state.visible());
    state.dismiss("another-service");
    assert!(state.visible());
    state.dismiss("service");
    for attempt in 2..20 {
        state.publish("service", notice(&format!("Attempt {attempt}")));
        assert!(!state.visible());
        assert!(state.area.is_none() && state.actions.is_empty());
    }
    state.show("service", notice("A new incident"));
    assert!(state.visible());
    state.remove("another-service");
    assert!(state.visible());
    state.remove("service");
    assert!(!state.visible());
}

#[test]
fn notice_is_compact_on_the_left_and_uses_dark_light_and_custom_theme_colors() {
    let mut app = app();
    for theme in [EditorTheme::Dark, EditorTheme::Light] {
        app.colors = theme.colors();
        let buffer = draw(&mut app, 110, 32);
        let area = app.side_notice.area.unwrap();
        assert_eq!((area.x, area.width, area.height), (1, 52, 5));
        assert_eq!(area.bottom(), app.status_bar_area.unwrap().y);
        assert!(text(&buffer).contains("Background service"));
        assert!(text(&buffer).contains("Waiting for the service"));
        assert_eq!(
            buffer[(area.x + 2, area.y + 2)].bg,
            app.colors.ui.bg_notification
        );
        assert!(
            app.side_notice
                .actions
                .iter()
                .all(|(hit, _)| area.contains((hit.x, hit.y).into()))
        );
    }
    app.colors.ui.bg_notification = Color::Rgb(22, 35, 49);
    app.colors.ui.fg_notification = Color::Rgb(236, 230, 218);
    app.colors.ui.border_notification = Some(Color::Rgb(89, 106, 125));
    let buffer = draw(&mut app, 110, 32);
    let area = app.side_notice.area.unwrap();
    assert_eq!(buffer[(area.x + 2, area.y + 2)].bg, Color::Rgb(22, 35, 49));
    assert_eq!(buffer[(area.x, area.y)].fg, Color::Rgb(89, 106, 125));
}

#[test]
fn ordinary_notifications_never_create_a_side_notice() {
    let mut app = App::test_new();
    app.notify("Saved");
    app.notify_warn("Background warning");
    app.notify_error("An ordinary error");
    assert!(!app.side_notice.visible());
    assert_eq!(app.notification_history.len(), 3);
}

#[test]
fn generic_action_dispatches_a_workbench_command_without_any_remote_workspace() {
    let mut app = app();
    assert!(app.remote_workspace.is_none());
    draw(&mut app, 110, 32);
    let (hit, _) = app.side_notice.actions[0];
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        hit.x,
        hit.y,
    );
    assert!(app.about_info.is_some());
    draw(&mut app, 110, 32);
    assert!(
        app.side_notice.area.is_none(),
        "the confirmation has priority"
    );
    app.about_info = None;
    draw(&mut app, 110, 32);
    assert!(app.side_notice.area.is_some());
}

#[test]
fn a_lifecycle_update_before_the_next_paint_blocks_stale_actions_and_click_through() {
    let mut app = app();
    draw(&mut app, 110, 32);
    let (old_action, _) = app.side_notice.actions[0];
    let before = app.editor().unwrap().rope.clone();
    let cursor = (
        app.editor().unwrap().cursor_line,
        app.editor().unwrap().cursor_col,
    );
    app.side_notice
        .publish("fixture", notice("Status changed before the next frame"));
    assert!(app.side_notice.area.is_some());
    assert!(app.side_notice.actions.is_empty());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        old_action.x,
        old_action.y,
    );
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 80, 5);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 80, 5);
    assert!(app.about_info.is_none());
    assert_eq!(app.editor().unwrap().rope, before);
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        cursor
    );
    draw(&mut app, 110, 32);
    let (action, _) = app.side_notice.actions[0];
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        action.x,
        action.y,
    );
    assert!(app.about_info.is_some());
}

#[test]
fn close_and_all_notice_body_gestures_keep_pointer_ownership_after_leaving_the_card() {
    for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
        for close in [false, true] {
            let mut app = app();
            app.editor_mut().unwrap().selection_anchor = Some((0, 2));
            app.editor_mut().unwrap().set_cursor_without_scrolling(3, 5);
            draw(&mut app, 110, 32);
            let code = app.code_area.unwrap();
            let editor = app.editor().unwrap();
            let before = (
                editor.rope.clone(),
                editor.cursor_line,
                editor.cursor_col,
                editor.selection_anchor,
                editor.scroll_offset,
            );
            let hit = if close {
                app.side_notice.close
            } else {
                app.side_notice.area.unwrap()
            };
            mouse(&mut app, MouseEventKind::Down(button), hit.x, hit.y);
            if close && button == MouseButton::Left {
                assert!(!app.side_notice.visible());
            }
            for (x, y) in [(code.x + 3, code.y + 2), (0, 0), (109, 31)] {
                mouse(&mut app, MouseEventKind::Drag(button), x, y);
            }
            mouse(&mut app, MouseEventKind::Up(button), code.x, code.y);
            let editor = app.editor().unwrap();
            assert_eq!(
                (
                    editor.rope.clone(),
                    editor.cursor_line,
                    editor.cursor_col,
                    editor.selection_anchor,
                    editor.scroll_offset
                ),
                before
            );
        }
    }
}

#[test]
fn notice_does_not_take_keyboard_focus_or_block_output() {
    let mut app = app();
    draw(&mut app, 110, 32);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert!(app.editor().unwrap().rope.to_string().starts_with('x'));
    assert_eq!(app.active_pane, ActivePane::Editor);
    app.show_output();
    draw(&mut app, 110, 32);
    assert!(app.side_notice.visible());
    assert!(crate::ui_logs::key(
        &mut app,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)
    ));
    assert_eq!(app.active_pane, ActivePane::Panel);
}

#[test]
fn higher_priority_overlays_and_resizing_cannot_leave_stale_notice_click_targets() {
    let mut app = app();
    draw(&mut app, 110, 32);
    let old_close = app.side_notice.close;
    app.exit_confirm_active = true;
    draw(&mut app, 110, 32);
    assert!(app.side_notice.area.is_none());
    assert!(!handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: old_close.x,
            row: old_close.y,
            modifiers: KeyModifiers::NONE
        }
    ));
    assert!(app.side_notice.visible());
    app.exit_confirm_active = false;
    for width in [0, 1, 10, 16, 24, 52, 110] {
        for height in [0, 1, 3, 6, 7, 32] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| super::draw(frame, &mut app)).unwrap();
            if let Some(area) = app.side_notice.area {
                assert!(area.right() <= width && area.bottom() <= height);
                assert!(area.contains((app.side_notice.close.x, app.side_notice.close.y).into()));
            } else {
                assert!(app.side_notice.actions.is_empty());
            }
        }
    }
}

#[test]
fn displaying_and_dismissing_a_notice_has_no_background_work_or_idle_deadline() {
    let mut app = app();
    let wake = app.background.wake_handle();
    let generation = wake.generation();
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    for _ in 0..20 {
        terminal.draw(|frame| super::draw(frame, &mut app)).unwrap();
        assert!(app.remote_connection.next_wait().is_none());
    }
    app.side_notice.dismiss("fixture");
    assert_eq!(wake.generation(), generation);
    assert!(app.remote_connection.next_wait().is_none());
}
