use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
};
use crossterm::event::KeyModifiers;
use ratatui::{Terminal, backend::TestBackend};

pub(super) fn draw(app: &mut App) {
    let mut terminal = Terminal::new(TestBackend::new(150, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

pub(super) fn fixture(tab: u8, selected: bool, split: bool) -> App {
    let mut app = App::test_new();
    app.source_control_enabled = false;
    app.show_explorer = false;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.show_panel = true;
    app.panel_tab = tab;
    app.panel_height = 12;
    let mut editor = Editor::test_with_text(&"let sample = 123;\n".repeat(300));
    editor.file_path = Some(app.explorer.root.join("panel-source.rs"));
    editor.word_wrap = false;
    app.tabs = vec![Tab::new_file(editor)];
    if split {
        app.tabs.push(Tab::new_file(Editor::test_with_text(
            &"other sample\n".repeat(300),
        )));
        app.create_split_group(1);
    }
    app.active_pane = ActivePane::Editor;
    let editor = app.editor_mut().unwrap();
    editor.scroll_offset = 3;
    editor.set_cursor_without_scrolling(10, 7);
    editor.selection_anchor = selected.then_some((5, 2));
    draw(&mut app);
    app
}

pub(super) type EditorState = (
    ropey::Rope,
    (usize, usize),
    Option<(usize, usize)>,
    (usize, usize, usize),
    u64,
);
pub(super) fn state(app: &App) -> EditorState {
    let editor = app.editor().unwrap();
    (
        editor.rope.clone(),
        (editor.cursor_line, editor.cursor_col),
        editor.selection_anchor,
        (
            editor.scroll_offset,
            editor.wrap_scroll_offset,
            editor.horizontal_scroll,
        ),
        editor.content_version,
    )
}
pub(super) fn mouse(app: &mut App, kind: MouseEventKind, position: (u16, u16)) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: position.0,
            row: position.1,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn panel_body_gestures_cannot_scroll_select_or_edit_the_file_even_after_leaving_the_panel() {
    for tab in [1, 2, 3] {
        for selected in [false, true] {
            for split in [false, true] {
                for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
                    let mut app = fixture(tab, selected, split);
                    let panel = app.panel_area.unwrap();
                    let code = app.code_area.unwrap();
                    let before = state(&app);
                    let origin = (panel.x + 5, panel.y + 2);
                    mouse(&mut app, MouseEventKind::Down(button), origin);
                    for point in [
                        origin,
                        (panel.x + 10, panel.bottom() - 1),
                        (code.x + 2, code.y + 1),
                        (0, 0),
                    ] {
                        mouse(&mut app, MouseEventKind::Drag(button), point);
                        assert_eq!(
                            state(&app),
                            before,
                            "tab {tab}, {selected:?}, {split:?}, {button:?}"
                        );
                    }
                    mouse(
                        &mut app,
                        MouseEventKind::Up(button),
                        (code.x + 2, code.y + 1),
                    );
                    assert_eq!(state(&app), before);
                }
            }
        }
    }
}

#[test]
fn problems_navigation_happens_once_without_turning_the_press_into_an_editor_drag() {
    let mut app = fixture(1, true, false);
    app.cached_problems = vec![(
        "panel-source.rs".into(),
        35,
        "TODO".into(),
        "Navigate here".into(),
        app.editor().unwrap().navigation_id,
    )];
    let panel = app.panel_area.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        (panel.x + 5, panel.y + 2),
    );
    assert_eq!(app.editor().unwrap().cursor_line, 34);
    assert!(app.editor().unwrap().selection_anchor.is_none());
    let navigated = state(&app);
    for row in [panel.bottom() - 1, panel.y + 2, 0] {
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            (panel.x + 8, row),
        );
        assert_eq!(state(&app), navigated);
    }
    let code = app.code_area.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        (code.x, code.y),
    );
    assert_eq!(state(&app), navigated);
}

#[test]
fn panel_header_gestures_keep_ownership_after_switching_maximizing_or_closing_the_panel() {
    for action in ["switch", "maximize", "close"] {
        let mut app = fixture(1, true, false);
        let hit = match action {
            "switch" => {
                app.panel_tab_areas
                    .iter()
                    .find(|(_, index)| *index == 3)
                    .unwrap()
                    .0
            }
            "maximize" => app.panel_chrome.maximize,
            "close" => app.panel_chrome.close,
            _ => unreachable!(),
        };
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (hit.x, hit.y),
        );
        draw(&mut app);
        if action == "switch" {
            assert_eq!(app.panel_tab, 3);
        }
        if action == "close" {
            assert!(!app.show_panel);
        }
        if action == "maximize" {
            assert!(app.panel_chrome.maximized);
        }
        let after_action = state(&app);
        let point = app
            .code_area
            .map_or((5, 5), |code| (code.x + 1, code.y + 1));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), point);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), point);
        assert_eq!(state(&app), after_action, "{action}");
    }
}

#[test]
fn a_fresh_editor_press_ends_panel_capture_and_editor_drags_can_cross_the_panel() {
    for release_panel in [false, true] {
        let mut app = fixture(3, false, false);
        let panel = app.panel_area.unwrap();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (panel.x + 5, panel.y + 2),
        );
        if release_panel {
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), (0, 0));
        }
        let code = app.code_area.unwrap();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (code.x + 1, code.y + 1),
        );
        let pressed = state(&app);
        for _ in 0..4 {
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                (code.x + 7, panel.y + 2),
            );
        }
        let editor = app.editor().unwrap();
        assert!(editor.scroll_offset > pressed.3.0);
        assert!(editor.has_selection());
        assert_eq!(editor.rope, pressed.0);
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            (code.x + 7, panel.y + 2),
        );
        assert!(app.drag_start_pos.is_none());
    }
}

#[test]
fn panel_capture_survives_context_menu_routing_and_ends_on_release() {
    use crate::app_event_loop::{TerminalEventPump, handle_mouse_input};
    let mut app = fixture(3, true, false);
    let panel = app.panel_area.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        (panel.x + 5, panel.y + 2),
    );
    app.context_menu_active = true;
    let before = state(&app);
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for kind in [
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        handle_mouse_input(
            &mut app,
            &mut pump,
            MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(state(&app), before);
    }
    assert!(app.panel_chrome.pointer.is_none());
}
