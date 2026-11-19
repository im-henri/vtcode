use super::*;
use crate::app::{Tab, TabTextColor};
use crate::editor::Editor;
use ratatui::{Terminal, backend::TestBackend};

fn app_with_tab_menu(anchor: (u16, u16)) -> App {
    let mut app = App::test_new();
    app.tabs = ["target.rs", "active.rs"]
        .into_iter()
        .map(|name| {
            let mut editor = Editor::new();
            editor.file_path = Some(name.into());
            Tab::new_file(editor)
        })
        .collect();
    app.active_tab = 1;
    app.tab_context_tab = 0;
    app.tab_context_pos = anchor;
    app.tab_context_active = true;
    app
}

fn render_menu(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui_chrome::draw_tab_context_menu(frame, app))
        .unwrap();
    terminal
}

// Locate labels in the rendered cells, independently of mouse hit geometry.
fn label_position(terminal: &Terminal<TestBackend>, label: &str) -> (u16, u16) {
    let buffer = terminal.backend().buffer();
    let width = label.len() as u16;
    (0..buffer.area.height)
        .find_map(|row| {
            (0..=buffer.area.width.saturating_sub(width)).find_map(|col| {
                let text: String = (col..col + width)
                    .map(|x| buffer[(x, row)].symbol())
                    .collect();
                (text == label).then_some((col, row))
            })
        })
        .unwrap_or_else(|| panic!("{label} must be visible"))
}

fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16)) {
    handle_mouse(
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
fn tab_text_color_menu_sets_and_resets_the_right_clicked_tab() {
    use crossterm::event::{KeyCode, KeyEvent};

    let mut app = app_with_tab_menu((0, 0));
    app.close_tab_context_menu();
    let strip = Rect::new(0, 0, 80, 1);
    app.tab_bar_area = Some(strip);
    let target = TabBarHits::new(&app, strip).tabs[0].1;
    let right_click = (target.x + 1, target.y);
    let choose = |app: &mut App| {
        mouse(app, MouseEventKind::Down(MouseButton::Right), right_click);
        let menu = render_menu(app, 80, 24);
        let color_command = label_position(&menu, "Tab Text Color");
        mouse(app, MouseEventKind::Down(MouseButton::Left), color_command);
    };
    let key = |app: &mut App, code| {
        crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
    };

    choose(&mut app);
    let blue = TabTextColor::CHOICES.iter()
        .position(|(color, _)| *color == Some(TabTextColor::Blue)).unwrap();
    for _ in 0..blue {
        key(&mut app, KeyCode::Down);
    }
    let clipped = render_menu(&mut app, 40, 7);
    assert!(app.tab_context_scroll > 0);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), label_position(&clipped, "Blue"));
    assert!(!app.tab_context_active);
    assert_eq!(app.tabs[0].text_color, Some(TabTextColor::Blue));
    assert_eq!(app.tabs[1].text_color, None);
    assert_eq!(app.active_tab, 1);
    assert!(!app.tabs[0].has_unsaved_changes());

    choose(&mut app);
    assert_eq!(app.tab_context_selected, blue);
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.tabs[0].text_color, Some(TabTextColor::Blue));

    choose(&mut app);
    for _ in 0..blue {
        key(&mut app, KeyCode::Up);
    }
    key(&mut app, KeyCode::Enter);
    assert!(!app.tab_context_active);
    assert_eq!(app.tabs[0].text_color, None);
    assert_eq!(app.active_tab, 1);
}

#[test]
fn tab_menu_clicks_follow_rendered_commands_at_window_edges() {
    for (width, height, x, y) in [
        (80, 40, 4, 3),
        (80, 40, 78, 38),
        (40, 32, 38, 30),
        (80, 24, 75, 1),
        (80, 40, u16::MAX, u16::MAX),
    ] {
        let mut app = app_with_tab_menu((x, y));
        let terminal = render_menu(&mut app, width, height);
        let (col, row) = label_position(&terminal, "Pin Tab");
        app.tab_context_selected = 0;
        mouse(&mut app, MouseEventKind::Moved, (col + 6, row));
        assert_eq!(
            App::tab_context_items()[app.tab_context_selected].1,
            "tab_pin"
        );
        // Click the end of the label to catch width as well as height mismatches.
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            (col + 6, row),
        );
        assert!(!app.tab_context_active, "{width}x{height}, anchor {x},{y}");
        assert_eq!(app.tabs.len(), 2);
        assert!(
            app.tabs[0].pinned,
            "Pin Tab at {width}x{height}, anchor {x},{y}"
        );
        assert_eq!(app.active_tab, 1);
    }
}

#[test]
fn tab_menu_border_and_secondary_buttons_do_not_execute_close() {
    for (kind, border) in [
        (MouseEventKind::Down(MouseButton::Left), true),
        (MouseEventKind::Down(MouseButton::Right), false),
        (MouseEventKind::Down(MouseButton::Middle), false),
    ] {
        let mut app = app_with_tab_menu((0, 0));
        let terminal = render_menu(&mut app, 80, 40);
        let (col, row) = label_position(&terminal, "Close");
        mouse(&mut app, kind, (col, row - u16::from(border)));
        assert_eq!(app.tabs.len(), 2, "{kind:?}, border={border}");
        assert!(!app.confirm_close_active);
        assert_eq!(app.tab_context_active, border);
    }
}

#[test]
fn tab_menu_duplicate_works_on_inactive_unsaved_tab_with_mouse_and_keyboard() {
    for keyboard in [false, true] {
        let mut app = app_with_tab_menu((0, 0));
        app.tabs[0].editor.rope = ropey::Rope::from_str("unsaved target\n");
        app.tabs[0].editor.modified = true;
        app.tabs[0].editor.cursor_col = 7;
        app.tabs[0].editor.saved_rope = Some(ropey::Rope::from_str("saved target\n"));
        app.tabs[0].editor.encoding = "UTF-16 LE";
        app.tabs[0].editor.content_version = 9;
        app.tabs[1].editor.rope = ropey::Rope::from_str("different active file\n");
        let source_id = app.tabs[0].editor.navigation_id;
        if keyboard {
            app.tab_context_selected = App::tab_context_items()
                .iter()
                .position(|item| item.1 == "duplicate_tab")
                .unwrap();
            crate::input_dialogs::handle_tab_context_key(
                &mut app,
                event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        } else {
            let terminal = render_menu(&mut app, 80, 40);
            let position = label_position(&terminal, "Duplicate Tab");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), position);
        }
        assert!(!app.tab_context_active);
        assert_eq!(app.tabs.len(), 3, "keyboard={keyboard}");
        assert_eq!(app.active_tab, 2);
        let duplicate = &app.tabs[2].editor;
        assert_eq!(duplicate.rope.to_string(), "unsaved target\n");
        assert_eq!(duplicate.file_path, app.tabs[0].editor.file_path);
        assert_eq!(duplicate.cursor_col, 7);
        assert!(duplicate.modified);
        assert_eq!(
            duplicate.saved_rope.as_ref().unwrap().to_string(),
            "saved target\n"
        );
        assert_eq!(duplicate.encoding, "UTF-16 LE");
        assert_eq!(duplicate.content_version, 9);
        assert_ne!(duplicate.navigation_id, source_id);
        assert_eq!(
            app.tabs[1].editor.rope.to_string(),
            "different active file\n"
        );
        app.sync_duplicate_file_tabs();
        assert_eq!(
            app.tabs[0].editor.saved_rope.as_ref().unwrap().to_string(),
            "saved target\n"
        );
    }
}

#[test]
fn tab_menu_resize_refreshes_click_targets() {
    let mut app = app_with_tab_menu((118, 48));
    render_menu(&mut app, 120, 50);
    let previous = app.tab_context_inner.unwrap();
    let terminal = render_menu(&mut app, 40, 32);
    assert_ne!(app.tab_context_inner.unwrap(), previous);
    let position = label_position(&terminal, "Pin Tab");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), position);
    assert!(app.tabs[0].pinned);
}

#[test]
fn tab_menu_disabled_rows_and_separators_keep_the_menu_open() {
    let mut app = app_with_tab_menu((0, 0));
    let terminal = render_menu(&mut app, 80, 40);
    let disabled = label_position(&terminal, "Compare with Selected");
    let inner = app.tab_context_inner.unwrap();
    let separator = App::tab_context_items()
        .iter()
        .position(|item| item.1.is_empty())
        .unwrap();
    for position in [
        disabled,
        (inner.x + 1, inner.y + separator as u16),
        (inner.x - 1, inner.y),
        (inner.right(), inner.y),
        (inner.x, inner.bottom()),
    ] {
        mouse(&mut app, MouseEventKind::Moved, position);
        assert_eq!(app.tab_context_selected, 0);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), position);
        assert!(app.tab_context_active);
        assert_eq!(app.tabs.len(), 2);
    }
}

#[test]
fn tab_menu_dismissal_and_scrolling_do_not_act_on_the_underlying_editor() {
    let mut app = app_with_tab_menu((45, 3));
    app.tab_bar_area = Some(Rect::new(0, 0, 80, 1));
    app.code_area = Some(Rect::new(0, 1, 80, 39));
    app.tabs[1].editor.rope = ropey::Rope::from_str(&"line\n".repeat(100));
    app.tabs[1].editor.scroll_offset = 10;
    let terminal = render_menu(&mut app, 80, 40);
    let position = label_position(&terminal, "Close");
    mouse(&mut app, MouseEventKind::ScrollDown, position);
    assert_eq!(app.tabs[1].editor.scroll_offset, 10);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), (3, 0));
    assert!(!app.tab_context_active);
    assert_eq!(
        app.active_tab, 1,
        "dismissal must not switch tabs underneath"
    );
    assert_eq!(app.tabs.len(), 2);
}

#[test]
fn tab_menu_commands_remain_clickable_over_the_find_input() {
    let mut app = app_with_tab_menu((36, 2));
    app.search_active = true;
    app.search_input_focused = false;
    app.search_query = "search text".into();
    let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
    terminal
        .draw(|frame| crate::ui_search_dialog::draw_search_bar(frame, &mut app))
        .unwrap();
    let input_position = label_position(&terminal, "search text");
    app.text_input_regions.clear();
    terminal
        .draw(|frame| {
            crate::ui_search_dialog::draw_search_bar(frame, &mut app);
            crate::ui_chrome::draw_tab_context_menu(frame, &mut app);
        })
        .unwrap();
    let position = label_position(&terminal, "Close");
    assert_eq!(position.1, input_position.1);
    assert!(
        position.0 >= input_position.0 && position.0 < input_position.0 + 11,
        "the menu command must overlap the rendered Find text"
    );
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), position);
    assert!(!app.tab_context_active);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].editor.file_name(), "active.rs");
    assert!(!app.search_input_focused);
    assert_eq!(app.search_query, "search text");
}

#[test]
fn tab_menu_split_direction_matches_the_command_for_mouse_and_keyboard() {
    use crate::app::{SplitDirection, SplitNode};

    for (label, command, direction, leaves) in [
        (
            "Split Right",
            "tab_split_right",
            SplitDirection::Horizontal,
            [0, 1],
        ),
        (
            "Split Down",
            "tab_split_down",
            SplitDirection::Vertical,
            [0, 1],
        ),
        (
            "Split Left",
            "tab_split_left",
            SplitDirection::Horizontal,
            [1, 0],
        ),
        (
            "Split Up",
            "tab_split_up",
            SplitDirection::Vertical,
            [1, 0],
        ),
    ] {
        for keyboard in [false, true] {
            let mut app = app_with_tab_menu((0, 0));
            let original = app.tabs[0].editor.navigation_id;
            let source_selection = app.tabs[1].editor.navigation_id;
            if keyboard {
                app.tab_context_selected = App::tab_context_items()
                    .iter()
                    .position(|item| item.1 == command)
                    .unwrap();
                crate::input_dialogs::handle_tab_context_key(
                    &mut app,
                    event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                );
            } else {
                let terminal = render_menu(&mut app, 80, 40);
                let position = label_position(&terminal, label);
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), position);
            }
            let Some(SplitNode::Split {
                direction: actual, ..
            }) = &app.split_tree
            else {
                panic!("{label} must create a split");
            };
            assert_eq!(*actual, direction, "{label}, keyboard={keyboard}");
            assert_eq!(app.split_tree.as_ref().unwrap().leaves(), leaves);
            assert_eq!(app.tabs[app.active_tab].editor.file_name(), "target.rs");
            let source = &app.groups[0];
            assert_eq!(source.tabs[source.active_tab].editor.navigation_id, source_selection);
            assert_eq!(source.tabs[0].editor.navigation_id, original);
            assert!(source.tabs[0].editor.shares_document(app.editor().unwrap()));
        }
    }
}

#[test]
fn duplicate_tab_preserves_clean_buffers_and_rejects_nontext_or_loading_tabs() {
    let mut app = app_with_tab_menu((0, 0));
    app.tab_context_active = false;
    app.active_tab = 0;
    app.tabs[0].editor.rope = ropey::Rope::from_str("saved text\n");
    app.tabs[0].editor.saved_rope = Some(app.tabs[0].editor.rope.clone());
    app.execute_palette_command("duplicate_tab");
    assert_eq!(app.tabs.len(), 3);
    assert!(!app.tabs[2].editor.modified);
    assert!(
        app.tabs[2]
            .editor
            .rope
            .is_instance(&app.tabs[0].editor.rope)
    );
    for kind in [
        crate::app::TabKind::Terminal,
        crate::app::TabKind::ExtensionView,
        crate::app::TabKind::File,
    ] {
        app.tabs[0].kind = kind;
        app.tabs[0].editor.bg_loading = kind == crate::app::TabKind::File;
        assert!(app.tab_context_command_disabled("duplicate_tab"));
        app.duplicate_tab(0);
        assert_eq!(app.tabs.len(), 3);
    }
}
