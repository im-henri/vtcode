use super::*;
use crate::app::{DiffMode, SplitDirection};
use crate::editor::Editor;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(&"line of text\n".repeat(100));
    editor.file_path = Some("minimap.txt".into());
    app.tabs.push(Tab::new_file(editor));
    app.show_explorer = false;
    app.show_activity_bar = false;
    app.show_minimap = true;
    app
}

fn draw(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
}

fn mouse(app: &mut App, kind: MouseEventKind, point: Position) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: point.x,
            row: point.y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn divider_top(minimap: Rect) -> Position {
    Position::new(minimap.x - 1, minimap.y)
}

fn click_button(app: &mut App, terminal: &mut Terminal<TestBackend>, button: Position) {
    mouse(app, MouseEventKind::Moved, button);
    draw(app, terminal);
    mouse(app, MouseEventKind::Down(MouseButton::Left), button);
    mouse(app, MouseEventKind::Drag(MouseButton::Left), button);
    mouse(app, MouseEventKind::Up(MouseButton::Left), button);
    draw(app, terminal);
}

#[test]
fn divider_lock_only_appears_on_hover_and_protects_width_in_each_layout() {
    for mode in [DiffMode::Off, DiffMode::Inline, DiffMode::SideBySide] {
        let mut app = fixture();
        if mode != DiffMode::Off {
            app.diff_mode = mode;
            app.compare_base_content = Some("original\n".into());
            app.diff_target_path = app.editor().unwrap().file_path.clone();
            app.set_compare_diff_lines(crate::app::compute_file_diff(
                "original\n",
                &app.editor().unwrap().rope.to_string(),
            ));
        }
        let mut terminal = Terminal::new(TestBackend::new(220, 32)).unwrap();
        draw(&mut app, &mut terminal);
        let initial = app.minimap_width;
        let button = divider_top(app.minimap_area.unwrap());
        let down = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let up = MouseEventKind::Up(MouseButton::Left);
        assert_eq!(terminal.backend().buffer()[button].symbol(), "│");

        mouse(
            &mut app,
            MouseEventKind::Moved,
            Position::new(button.x, button.y + 2),
        );
        assert!(app.mouse_hover_changed);
        draw(&mut app, &mut terminal);
        assert_eq!(terminal.backend().buffer()[button].symbol(), "L");
        mouse(
            &mut app,
            MouseEventKind::Moved,
            Position::new(button.x - 1, button.y + 2),
        );
        assert!(app.mouse_hover_changed);
        draw(&mut app, &mut terminal);
        assert_eq!(terminal.backend().buffer()[button].symbol(), "│");

        // Dragging from the locked control must neither unlock nor resize.
        mouse(&mut app, MouseEventKind::Moved, button);
        draw(&mut app, &mut terminal);
        let end = Position::new(button.x - 11, 31);
        mouse(&mut app, down, button);
        mouse(&mut app, drag, end);
        mouse(&mut app, up, end);
        assert!(app.minimap_resize.locked);
        assert_eq!(app.minimap_width, initial);
        assert_eq!(app.settings_global.minimap_width, None);

        click_button(&mut app, &mut terminal, button);
        assert_eq!(terminal.backend().buffer()[button].symbol(), "↔");
        mouse(&mut app, down, Position::new(button.x, button.y + 2));
        mouse(&mut app, drag, end);
        draw(&mut app, &mut terminal);
        assert_eq!(app.minimap_area.unwrap().width, initial + 11);
        assert_eq!(app.minimap_width, initial);
        assert_eq!(app.settings_global.minimap_width, None);
        mouse(&mut app, up, end);
        assert_eq!(app.minimap_width, initial + 11);
        assert_eq!(app.settings_global.minimap_width, Some(initial + 11));

        let button = divider_top(app.minimap_area.unwrap());
        click_button(&mut app, &mut terminal, button);
        assert!(app.minimap_resize.locked);
        assert_eq!(terminal.backend().buffer()[button].symbol(), "L");
    }
}

#[test]
fn inactive_split_resize_keeps_focus_clamps_width_and_can_be_cancelled() {
    let mut app = fixture();
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("other pane")));
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    let active = app.active_group;
    let inactive = usize::from(active == 0);
    let mut terminal = Terminal::new(TestBackend::new(220, 32)).unwrap();
    draw(&mut app, &mut terminal);
    let button = divider_top(app.groups[inactive].minimap_area.unwrap());
    click_button(&mut app, &mut terminal, button);
    let down = MouseEventKind::Down(MouseButton::Left);
    let drag = MouseEventKind::Drag(MouseButton::Left);
    let up = MouseEventKind::Up(MouseButton::Left);
    let start = Position::new(button.x, button.y + 1);
    let left = Position::new(0, start.y);
    mouse(&mut app, down, start);
    mouse(&mut app, drag, left);
    mouse(&mut app, up, left);
    assert_eq!(app.active_group, active);
    assert_eq!(app.minimap_width, MAX_MINIMAP_WIDTH);
    draw(&mut app, &mut terminal);
    assert_eq!(app.minimap_area.unwrap().width, MAX_MINIMAP_WIDTH);
    let minimap = app.groups[inactive].minimap_area.unwrap();
    assert_eq!(minimap.width, MAX_MINIMAP_WIDTH);

    let start = Position::new(minimap.x - 1, minimap.y + 1);
    let right = Position::new(219, start.y);
    mouse(&mut app, down, start);
    mouse(&mut app, drag, right);
    assert_eq!(app.minimap_render_width(), MIN_MINIMAP_WIDTH);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    mouse(&mut app, up, right);
    assert_eq!(app.minimap_render_width(), MAX_MINIMAP_WIDTH);
    assert_eq!(app.settings_global.minimap_width, Some(MAX_MINIMAP_WIDTH));
}
