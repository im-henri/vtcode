use super::*;
use crate::{app::Tab, editor::Editor, mouse_overlay::handle_context_menu_mouse};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

const SOURCE: &str = "alpha beta\nsecond line\n";

fn fixture(x: u16, y: u16) -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(SOURCE))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.context_menu_active = true;
    app.context_menu_pos = (x, y);
    app.context_menu_items = App::build_context_menu();
    app
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_context_menu(frame, app))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn label_position(buffer: &Buffer, label: &str) -> (u16, u16) {
    (buffer.area.y..buffer.area.bottom())
        .find_map(|y| {
            (buffer.area.x..buffer.area.right().saturating_sub(label.len() as u16)).find_map(|x| {
                let text: String = (x..x + label.len() as u16)
                    .map(|col| buffer[(col, y)].symbol())
                    .collect();
                (text == label).then_some((x, y))
            })
        })
        .unwrap_or_else(|| panic!("{label:?} should be visible in {:?}", buffer.area))
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    handle_context_menu_mouse(
        app,
        event::MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn context_menu_visible_actions_work_when_shifted_flipped_or_clipped() {
    for (width, height, x, y, label, command) in [
        (100, 45, 3, 2, "Select All", "select_all"),
        (80, 40, 75, 35, "Select All", "select_all"),
        (50, 26, 49, 23, "Command Palette", "command_palette"),
        (44, 18, 40, 14, "Select Line", "select_line"),
        (24, 12, 20, 5, "Select All", "select_all"),
        (80, 40, u16::MAX, u16::MAX, "Select All", "select_all"),
    ] {
        for offset in [0, label.len() as u16 - 1] {
            let mut app = fixture(x, y);
            let buffer = render(&mut app, width, height);
            let (col, row) = label_position(&buffer, label);
            let col = col + offset;
            let expected = app
                .context_menu_items
                .iter()
                .position(|item| item.id == command)
                .unwrap();
            app.context_menu_selected = usize::MAX;
            mouse(&mut app, MouseEventKind::Moved, col, row);
            assert_eq!(
                app.context_menu_selected, expected,
                "{label} at {width}x{height}, anchor {x},{y}"
            );
            assert_eq!((app.mouse_col, app.mouse_row), (col, row));
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), col, row);
            assert!(!app.context_menu_active);
            assert_eq!(app.editor().unwrap().rope.to_string(), SOURCE);
            match command {
                "select_all" => assert_eq!(
                    app.editor().unwrap().selected_text().as_deref(),
                    Some(SOURCE)
                ),
                "select_line" => assert_eq!(
                    app.editor().unwrap().selected_text().as_deref(),
                    Some("alpha beta\n")
                ),
                "command_palette" => assert!(app.palette.active),
                _ => unreachable!(),
            }
        }
    }
}

#[test]
fn context_menu_borders_and_separators_never_activate_commands() {
    let mut app = fixture(75, 35);
    render(&mut app, 80, 40);
    let inner = app.context_menu_inner.unwrap();
    let selected = 4;
    app.context_menu_selected = selected;
    for (col, row) in [
        (inner.x - 1, inner.y),
        (inner.x + 1, inner.y - 1),
        (inner.right(), inner.y + 1),
        (inner.x + 1, inner.bottom()),
        (inner.x + 1, inner.y + 3), // Separator after Paste.
    ] {
        mouse(&mut app, MouseEventKind::Moved, col, row);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), col, row);
        assert!(app.context_menu_active, "border/separator ({col}, {row})");
        assert_eq!(app.context_menu_selected, selected);
        assert_eq!(app.editor().unwrap().rope.to_string(), SOURCE);
        assert!(!app.editor().unwrap().has_selection());
    }
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
    assert!(
        app.context_menu_active,
        "release outside keeps the menu open"
    );
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
    assert!(
        !app.context_menu_active,
        "press outside dismisses without an editor action"
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), SOURCE);
}

#[test]
fn context_menu_uses_new_bounds_after_resizing_and_drops_invisible_rows() {
    let mut app = fixture(75, 35);
    let large = render(&mut app, 100, 45);
    let old_bounds = app.context_menu_inner;
    let hidden_action = label_position(&large, "Command Palette");
    let small = render(&mut app, 40, 16);
    assert_ne!(app.context_menu_inner, old_bounds);
    let (col, row) = label_position(&small, "Select All");
    mouse(&mut app, MouseEventKind::Moved, col, row);
    assert_eq!(
        app.context_menu_items[app.context_menu_selected].id,
        "select_all"
    );
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        hidden_action.0,
        hidden_action.1,
    );
    assert!(!app.context_menu_active);
    assert!(
        !app.palette.active,
        "a clipped command cannot be activated at its old position"
    );

    app.context_menu_active = true;
    render(&mut app, 2, 2);
    assert!(app.context_menu_inner.is_none());
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), col, row);
    assert!(!app.context_menu_active);
    assert!(!app.editor().unwrap().has_selection());
}

#[test]
fn reopened_context_menu_does_not_reuse_previous_hit_regions_before_drawing() {
    let mut app = fixture(2, 2);
    render(&mut app, 80, 40);
    assert!(app.context_menu_inner.is_some());
    app.open_source_control_action_menu(2, 2);
    assert!(app.context_menu_inner.is_none());

    render(&mut app, 80, 40);
    app.context_menu_active = false;
    app.code_area = Some(Rect::new(0, 0, 80, 40));
    handle_mouse(
        &mut app,
        event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.context_menu_active);
    assert!(app.context_menu_inner.is_none());
}
