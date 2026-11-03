use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
    terminal::TerminalEmulator,
};
use crossterm::event::KeyModifiers;
use ratatui::{Terminal, backend::TestBackend, style::Color};
use std::sync::{Arc, Mutex, mpsc};

struct Fixture {
    app: App,
    input: Arc<Mutex<Vec<u8>>>,
    _running: mpsc::Sender<()>,
}

impl Fixture {
    fn new() -> Self {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(
            "keep the document\nsecond line",
        ))];
        app.show_explorer = false;
        app.show_panel = false;
        app.show_terminal_sidebar = true;
        app.terminal_sidebar_revealed = true;
        app.active_pane = ActivePane::Editor;
        app.terminal_panel_focused = true;
        let mut pty =
            TerminalEmulator::test_with_output(20, 3, "\x1b[?1003h\x1b[?1006hhello\r\nworld");
        pty.process();
        let running = pty.test_keep_running();
        let input = pty.test_capture_input();
        app.terminals[0].is_placeholder = false;
        app.terminals[0].terminal_pty = Some(pty);
        Self {
            app,
            input,
            _running: running,
        }
    }

    fn draw(&mut self, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut self.app))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn mouse(&mut self, kind: MouseEventKind, column: u16, row: u16) {
        crate::mouse::handle_mouse(
            &mut self.app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
}

#[test]
fn widths_keep_the_responsive_default_and_clamp_manual_sizes_without_losing_editing_space() {
    assert_eq!(width(100, true, None), 34);
    assert_eq!(width(200, true, None), 56);
    assert_eq!(width(100, true, Some(72)), 72);
    assert_eq!(width(100, true, Some(1)), 12);
    assert_eq!(width(100, true, Some(u16::MAX)), 76);
    for available in [0, 1, 2, 3, 6, 12, 30, 80, 200, u16::MAX] {
        for requested in [0, 12, 72, u16::MAX] {
            let size = width(available, true, Some(requested));
            assert!(size <= available.saturating_sub(1));
            if available >= 36 {
                assert!(available - size >= 24);
            }
            assert_eq!(
                width(available, false, Some(requested)),
                3.min(available.saturating_sub(1))
            );
        }
    }
}

#[test]
fn dragging_the_rendered_divider_resizes_the_terminal_and_owns_the_entire_mouse_gesture() {
    for sidebar_right in [false, true] {
        for row_offset in [0, 1, 8] {
            let mut fixture = Fixture::new();
            fixture.app.show_explorer = true;
            fixture.app.sidebar_right = sidebar_right;
            let buffer = fixture.draw(140, 34);
            let body = fixture.app.workbench_body_area.unwrap();
            let handle = fixture.app.terminal_sidebar.handle.unwrap();
            assert_eq!(buffer[(handle.x, handle.y)].symbol(), "│");
            for &(area, _) in fixture
                .app
                .terminal_sidebar_tab_areas
                .iter()
                .chain(&fixture.app.terminal_sidebar_close_areas)
            {
                assert!(area.intersection(handle).is_empty());
            }
            assert!(
                fixture
                    .app
                    .terminal_sidebar_toggle_area
                    .unwrap()
                    .intersection(handle)
                    .is_empty()
            );
            let before = fixture.app.editor().unwrap().rope.clone();
            let row = handle.y + row_offset;
            fixture.mouse(MouseEventKind::Down(MouseButton::Left), handle.x, row);
            assert!(fixture.app.terminal_sidebar.dragging());
            assert!(
                fixture.app.terminal_panel_focused,
                "resizing preserves terminal keyboard focus"
            );
            fixture.mouse(MouseEventKind::Drag(MouseButton::Left), handle.x, row);
            assert_eq!(
                fixture.app.terminal_sidebar.width, None,
                "a stationary press keeps automatic sizing"
            );
            fixture.mouse(
                MouseEventKind::Drag(MouseButton::Left),
                body.right() - 76,
                body.bottom() + 1,
            );
            assert_eq!(fixture.app.terminal_sidebar.width, Some(76));
            fixture.draw(140, 34);
            let dock = fixture.app.terminal_sidebar_area.unwrap();
            let surface = fixture
                .app
                .terminal_surfaces
                .iter()
                .find(|surface| surface.group.is_none())
                .unwrap();
            assert_eq!(dock.width, 76);
            assert_eq!(surface.area.x, dock.x + 1);
            assert_eq!(surface.area.width, 75);
            assert_eq!(
                fixture.app.terminals[0]
                    .terminal_pty
                    .as_ref()
                    .unwrap()
                    .grid
                    .width,
                75
            );
            // The release can contain the final movement without a preceding drag.
            fixture.mouse(
                MouseEventKind::Up(MouseButton::Left),
                body.right() - 68,
                row,
            );
            assert_eq!(fixture.app.terminal_sidebar.width, Some(68));
            assert!(!fixture.app.terminal_sidebar.dragging());
            assert!(fixture.app.drag_start_pos.is_none());
            fixture.draw(140, 34);
            assert_eq!(fixture.app.terminal_sidebar_area.unwrap().width, 68);
            assert_eq!(
                fixture.app.terminals[0]
                    .terminal_pty
                    .as_ref()
                    .unwrap()
                    .grid
                    .width,
                67
            );
            assert_eq!(fixture.app.editor().unwrap().rope, before);
            assert!(fixture.app.editor().unwrap().selection_anchor.is_none());
            assert!(fixture.app.terminal_mouse_capture.is_none());
            assert!(fixture.app.terminal_selection_drag.is_none());
            assert!(
                fixture.input.lock().unwrap().is_empty(),
                "resize gestures must not reach mouse-aware terminal applications"
            );
        }
    }
}

#[test]
fn collapse_reopen_and_window_resize_preserve_the_requested_width() {
    let mut fixture = Fixture::new();
    fixture.app.terminal_sidebar.width = Some(74);
    fixture.draw(140, 32);
    assert_eq!(fixture.app.terminal_sidebar_area.unwrap().width, 74);
    fixture.app.toggle_terminal_sidebar();
    fixture.draw(140, 32);
    let collapsed = fixture.app.terminal_sidebar_area.unwrap();
    assert_eq!(collapsed.width, 3);
    assert!(fixture.app.terminal_sidebar.handle.is_none());
    fixture.mouse(
        MouseEventKind::Down(MouseButton::Left),
        collapsed.x + 1,
        collapsed.y + 3,
    );
    fixture.mouse(
        MouseEventKind::Up(MouseButton::Left),
        collapsed.x + 1,
        collapsed.y + 3,
    );
    fixture.draw(140, 32);
    assert_eq!(fixture.app.terminal_sidebar_area.unwrap().width, 74);
    assert!(!fixture.app.terminal_sidebar.dragging());
    fixture.draw(40, 16);
    assert!(fixture.app.terminal_sidebar_area.unwrap().width < 74);
    assert_eq!(fixture.app.terminal_sidebar.width, Some(74));
    fixture.draw(140, 32);
    assert_eq!(fixture.app.terminal_sidebar_area.unwrap().width, 74);
}

#[test]
fn disappearing_docks_keep_capture_until_release_and_focus_resets_cancel_resize() {
    let mut fixture = Fixture::new();
    fixture.draw(120, 30);
    let handle = fixture.app.terminal_sidebar.handle.unwrap();
    fixture.mouse(
        MouseEventKind::Down(MouseButton::Left),
        handle.x,
        handle.y + 3,
    );
    fixture.app.toggle_terminal_sidebar();
    fixture.draw(120, 30);
    fixture.mouse(MouseEventKind::Drag(MouseButton::Left), 10, 10);
    assert_eq!(fixture.app.terminal_sidebar.width, None);
    fixture.mouse(MouseEventKind::Up(MouseButton::Left), 10, 10);
    assert!(!fixture.app.terminal_sidebar.dragging());
    assert!(fixture.input.lock().unwrap().is_empty());
    fixture.app.toggle_terminal_sidebar();
    fixture.draw(120, 30);
    let handle = fixture.app.terminal_sidebar.handle.unwrap();
    fixture.mouse(
        MouseEventKind::Down(MouseButton::Left),
        handle.x,
        handle.y + 3,
    );
    fixture.app.clear_drag_states();
    assert!(!fixture.app.terminal_sidebar.dragging());
    assert!(fixture.app.drag_start_pos.is_none());
    fixture.app.zen_mode = true;
    fixture.draw(120, 30);
    assert!(fixture.app.terminal_sidebar.handle.is_none());
}

#[test]
fn divider_hover_is_visible_and_leaving_it_repaints_the_editor() {
    let mut fixture = Fixture::new();
    let normal = Color::Rgb(0, 255, 255);
    let hovered = Color::Rgb(255, 0, 255);
    fixture.app.colors.ui.panel_border = crate::theme_workbench::ColorLayer::opaque(normal);
    fixture.app.colors.ui.sash_hover_border = crate::theme_workbench::ColorLayer::opaque(hovered);
    fixture.app.mouse_col = u16::MAX;
    fixture.app.mouse_row = u16::MAX;
    let buffer = fixture.draw(120, 30);
    let handle = fixture.app.terminal_sidebar.handle.unwrap();
    let row = handle.y + 4;
    assert_eq!(buffer[(handle.x, row)].fg, normal);
    fixture.mouse(MouseEventKind::Moved, handle.x, row);
    assert_eq!(fixture.draw(120, 30)[(handle.x, row)].fg, hovered);
    let code = fixture.app.code_area.unwrap();
    fixture.app.mouse_hover_changed = false;
    fixture.mouse(MouseEventKind::Moved, code.x + 1, code.y + 1);
    assert!(fixture.app.mouse_hover_changed);
    assert_eq!(fixture.draw(120, 30)[(handle.x, row)].fg, normal);
}

#[test]
fn workspace_sessions_and_desks_remember_widths_and_old_layouts_keep_automatic_sizing() {
    let mut fixture = Fixture::new();
    fixture.app.terminal_sidebar.width = Some(74);
    let session = fixture.app.build_session();
    let mut json = serde_json::to_value(&session).unwrap();
    assert_eq!(json["terminalSidebarWidth"], 74);
    let loaded: crate::session::Session = serde_json::from_value(json.clone()).unwrap();
    let mut restored = App::test_new();
    restored.restore_session(&loaded);
    assert_eq!(restored.terminal_sidebar.width, Some(74));
    json.as_object_mut().unwrap().remove("terminalSidebarWidth");
    let old: crate::session::Session = serde_json::from_value(json).unwrap();
    assert_eq!(old.terminal_sidebar_width, None);

    let first = fixture.app.desks.active;
    let second = fixture.app.new_desk("Other", false);
    fixture.app.terminal_sidebar.width = Some(36);
    fixture.app.switch_desk(first);
    assert_eq!(fixture.app.terminal_sidebar.width, Some(74));
    fixture.app.switch_desk(second);
    assert_eq!(fixture.app.terminal_sidebar.width, Some(36));
    let mut layout = serde_json::to_value(fixture.app.capture_desk_layout()).unwrap();
    layout.as_object_mut().unwrap().remove("terminal_width");
    let old: crate::app_desks::DeskLayout = serde_json::from_value(layout).unwrap();
    assert_eq!(old.terminal_width, None);
}
