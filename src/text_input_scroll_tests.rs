use super::context_menu_tests::{EXTRA_FIELDS, field_app};
use super::tests::FIELDS;
use super::*;
use crate::ui_widgets::InputField;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use unicode_segmentation::UnicodeSegmentation;

fn state(app: &mut App, id: TextInputId) -> (String, usize, Option<usize>) {
    let (text, cursor, anchor) = app.text_input_state_mut(id);
    (text.clone(), *cursor, *anchor)
}

fn set(app: &mut App, id: TextInputId, value: &str) {
    let (text, cursor, anchor) = app.text_input_state_mut(id);
    *text = value.into();
    *cursor = value.len();
    *anchor = Some(0);
}

fn field(app: &mut App, id: TextInputId, width: u16) -> Buffer {
    app.begin_text_input_frame();
    let view = app.text_input_viewport(id);
    let colors = app.colors;
    let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
    let mut hit = None;
    terminal
        .draw(|frame| {
            let (text, cursor, anchor) = app.text_input_state_mut(id);
            hit = Some(
                InputField::new(text)
                    .viewport(view)
                    .theme_colors(&colors)
                    .prefix("⌕ ", colors.fg_text)
                    .suffix(" hint", colors.fg_dim)
                    .cursor_pos(*cursor)
                    .selection_range(App::inline_selection_range(*cursor, *anchor))
                    .render(frame, Rect::new(5, 3, width, 1)),
            );
        })
        .unwrap();
    app.register_text_input(id, hit.unwrap());
    terminal.backend().buffer().clone()
}

fn start(app: &App, id: TextInputId) -> usize {
    app.text_input_regions
        .iter()
        .rev()
        .find(|region| region.id == id)
        .unwrap()
        .hit_map
        .viewport()
        .unwrap()
        .start
}

fn wheel(app: &mut App, kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers,
        },
    );
}

#[test]
fn every_shared_single_line_field_scrolls_without_changing_text_caret_selection_or_focus() {
    for &id in FIELDS
        .iter()
        .chain(EXTRA_FIELDS)
        .filter(|id| !matches!(id, TextInputId::SourceControlCommit | TextInputId::Chat))
    {
        let mut app = field_app(id);
        set(&mut app, id, &"0123456789".repeat(20));
        field(&mut app, id, 24);
        let original = state(&mut app, id);
        let focus = app.active_pane;
        let initial = start(&app, id);
        assert!(initial > 0, "{id:?}");
        for (left, right) in [
            (MouseEventKind::ScrollUp, MouseEventKind::ScrollDown),
            (MouseEventKind::ScrollLeft, MouseEventKind::ScrollRight),
        ] {
            assert!(
                app.handle_text_input_mouse(left, 10, 3, KeyModifiers::NONE),
                "{id:?}"
            );
            field(&mut app, id, 24);
            assert!(start(&app, id) < initial, "{id:?}");
            assert_eq!(state(&mut app, id), original, "{id:?}");
            assert_eq!(app.active_pane, focus, "{id:?}");
            assert!(app.handle_text_input_mouse(right, 10, 3, KeyModifiers::SHIFT));
            field(&mut app, id, 24);
            assert_eq!(start(&app, id), initial, "{id:?}");
        }
    }
}

#[test]
fn queued_wheel_events_clamp_at_both_ends_and_reverse_without_a_repaint() {
    let id = TextInputId::SearchFind;
    let mut app = field_app(id);
    set(&mut app, id, &"0123456789".repeat(20));
    field(&mut app, id, 24);
    for _ in 0..100 {
        app.handle_text_input_mouse(MouseEventKind::ScrollUp, 10, 3, KeyModifiers::NONE);
    }
    field(&mut app, id, 24);
    assert_eq!(start(&app, id), 0);
    app.handle_text_input_mouse(MouseEventKind::ScrollUp, 10, 3, KeyModifiers::NONE);
    app.handle_text_input_mouse(MouseEventKind::ScrollDown, 10, 3, KeyModifiers::NONE);
    field(&mut app, id, 24);
    assert!(start(&app, id) > 0);
    for _ in 0..100 {
        app.handle_text_input_mouse(MouseEventKind::ScrollRight, 10, 3, KeyModifiers::NONE);
    }
    field(&mut app, id, 24);
    let end = start(&app, id);
    app.handle_text_input_mouse(MouseEventKind::ScrollRight, 10, 3, KeyModifiers::NONE);
    app.handle_text_input_mouse(MouseEventKind::ScrollLeft, 10, 3, KeyModifiers::NONE);
    field(&mut app, id, 24);
    assert!(start(&app, id) < end);
}

#[test]
fn unicode_scrolling_rebuilds_pointer_targets_and_preserves_the_view_when_clicking() {
    let id = TextInputId::SearchFind;
    let mut app = field_app(id);
    let text = "a界e\u{301}👩‍💻z".repeat(30);
    set(&mut app, id, &text);
    field(&mut app, id, 32);
    for _ in 0..5 {
        app.handle_text_input_mouse(MouseEventKind::ScrollUp, 10, 3, KeyModifiers::NONE);
    }
    let buffer = field(&mut app, id, 32);
    let first = start(&app, id);
    assert!(text.is_char_boundary(first));
    assert!(text.grapheme_indices(true).any(|(byte, _)| byte == first));
    let hit = &app.text_input_regions.last().unwrap().hit_map;
    let mut click = None;
    for x in 5..37 {
        let glyph = buffer[(x, 3)].symbol();
        let byte = hit.byte_at(x, 3);
        if byte >= first && byte < text.len() && !glyph.trim().is_empty() && glyph != "…" {
            assert!(text[byte..].starts_with(glyph), "{byte}: {glyph:?}");
            if glyph == "界" {
                click = Some((x, byte));
            }
        }
    }
    let (x, byte) = click.unwrap();
    assert!(app.handle_text_input_mouse(
        MouseEventKind::Down(MouseButton::Left),
        x,
        3,
        KeyModifiers::NONE
    ));
    assert!(app.handle_text_input_mouse(
        MouseEventKind::Up(MouseButton::Left),
        x,
        3,
        KeyModifiers::NONE
    ));
    assert_eq!(state(&mut app, id), (text, byte, None));
    field(&mut app, id, 32);
    assert_eq!(start(&app, id), first);
}

#[test]
fn typing_and_navigation_reveal_the_caret_without_adding_wheel_movements_to_undo() {
    let id = TextInputId::SearchFind;
    let mut app = field_app(id);
    let original = "0123456789".repeat(20);
    set(&mut app, id, &original);
    app.text_input_state_mut(id).2.take();
    app.edit_text_input(id, KeyCode::Char('Z'), KeyModifiers::NONE, None);
    field(&mut app, id, 24);
    for _ in 0..100 {
        app.handle_text_input_mouse(MouseEventKind::ScrollLeft, 10, 3, KeyModifiers::NONE);
    }
    let buffer = field(&mut app, id, 24);
    assert_eq!(start(&app, id), 0);
    assert!(
        (7..20).all(|x| !buffer[(x, 3)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD)),
        "offscreen caret must not be drawn at the clipped edge"
    );
    app.edit_text_input(id, KeyCode::End, KeyModifiers::NONE, None);
    field(&mut app, id, 24);
    assert!(start(&app, id) > 0);
    app.edit_text_input(id, KeyCode::Char('z'), KeyModifiers::CONTROL, None);
    assert_eq!(
        state(&mut app, id),
        (original.clone(), original.len(), None)
    );
}

#[test]
fn resize_replacement_and_workspace_changes_cannot_leave_a_stale_viewport() {
    let id = TextInputId::SearchFind;
    let mut app = field_app(id);
    set(&mut app, id, &"0123456789".repeat(5));
    field(&mut app, id, 24);
    app.handle_text_input_mouse(MouseEventKind::ScrollUp, 10, 3, KeyModifiers::NONE);
    field(&mut app, id, 12);
    assert!(app.text_input_viewport(id).is_some());
    field(&mut app, id, 80);
    assert_eq!(start(&app, id), 0);
    field(&mut app, id, 24);
    set(&mut app, id, &"abcdefghij".repeat(5));
    field(&mut app, id, 24);
    assert!(app.text_input_viewport(id).is_none());
    assert!(start(&app, id) > 0);
    app.handle_text_input_mouse(MouseEventKind::ScrollUp, 10, 3, KeyModifiers::NONE);
    app.workspace_generation += 1;
    field(&mut app, id, 24);
    assert!(app.text_input_viewport(id).is_none());
}

#[test]
fn real_find_sidebar_settings_and_dialog_fields_own_their_wheel_events() {
    for id in [
        TextInputId::SearchFind,
        TextInputId::SearchReplace,
        TextInputId::GlobalSearch(0),
        TextInputId::GlobalSearch(1),
        TextInputId::GlobalSearch(2),
        TextInputId::GlobalSearch(3),
        TextInputId::SettingsSearch,
        TextInputId::SaveAs,
        TextInputId::WorkspaceFilter,
        TextInputId::WorkspaceName,
    ] {
        let mut app = field_app(id);
        if id == TextInputId::SettingsSearch {
            app.open_settings_tab();
            app.settings_focus_categories = false;
        }
        set(&mut app, id, &"0123456789".repeat(50));
        if matches!(id, TextInputId::SearchFind | TextInputId::SearchReplace) {
            app.show_explorer = false;
        }
        if matches!(id, TextInputId::GlobalSearch(_)) {
            app.global_search_replace_visible = true;
            app.show_explorer = true;
            app.sidebar_view = crate::app::SidebarView::Search;
            app.active_pane = crate::app::ActivePane::Explorer;
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 60)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let area = app
            .text_input_area(id)
            .unwrap_or_else(|| panic!("{id:?} not rendered"));
        let initial = start(&app, id);
        let saved = state(&mut app, id);
        let editor_scroll = app.editor().map(|editor| editor.scroll_offset);
        let sidebar_scroll = app.explorer.scroll_offset;
        wheel(
            &mut app,
            MouseEventKind::ScrollUp,
            area.x + 2,
            area.y,
            KeyModifiers::NONE,
        );
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(
            start(&app, id) < initial,
            "{id:?}: {initial} -> {}",
            start(&app, id)
        );
        assert_eq!(state(&mut app, id), saved, "{id:?}");
        assert_eq!(
            app.editor().map(|editor| editor.scroll_offset),
            editor_scroll,
            "{id:?}"
        );
        assert_eq!(app.explorer.scroll_offset, sidebar_scroll, "{id:?}");
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(),
            &format!("Scrolled inline field {id:?}"),
        );
    }
}

#[test]
fn an_inactive_split_find_bar_scrolls_without_stealing_focus_or_the_other_views_offset() {
    use crate::app::{ActivePane, SplitDirection, Tab};
    use crate::editor::Editor;
    let mut app = App::test_new();
    app.tabs = vec![
        Tab::new_file(Editor::test_with_text("left\n")),
        Tab::new_file(Editor::test_with_text("right\n")),
    ];
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.search_active = true;
    app.search_input_focused = true;
    set(&mut app, TextInputId::SearchFind, &"left-query".repeat(40));
    let left_owner = app.file_search_owner();
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    app.search_active = true;
    app.search_input_focused = true;
    set(&mut app, TextInputId::SearchFind, &"right-query".repeat(40));
    let right_owner = app.file_search_owner();
    let mut terminal = Terminal::new(TestBackend::new(200, 45)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let left = app
        .text_input_regions
        .iter()
        .find(|region| {
            region.id == TextInputId::SearchFind && region.file_search_owner == left_owner
        })
        .unwrap();
    let area = left.hit_map.area;
    let before = left.hit_map.viewport().unwrap().start;
    let right_before = app
        .text_input_regions
        .iter()
        .find(|region| {
            region.id == TextInputId::SearchFind && region.file_search_owner == right_owner
        })
        .unwrap()
        .hit_map
        .viewport()
        .unwrap()
        .start;
    let group = app.active_group;
    wheel(
        &mut app,
        MouseEventKind::ScrollLeft,
        area.x + 2,
        area.y,
        KeyModifiers::NONE,
    );
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let left = app
        .text_input_regions
        .iter()
        .find(|region| {
            region.id == TextInputId::SearchFind && region.file_search_owner == left_owner
        })
        .unwrap();
    assert!(left.hit_map.viewport().unwrap().start < before);
    assert_eq!(app.file_search_owner(), right_owner);
    assert_eq!(app.active_group, group);
    let right_after = app
        .text_input_regions
        .iter()
        .find(|region| {
            region.id == TextInputId::SearchFind && region.file_search_owner == right_owner
        })
        .unwrap()
        .hit_map
        .viewport()
        .unwrap()
        .start;
    assert_eq!(right_after, right_before);
}

#[test]
fn popup_and_output_fields_scroll_through_workbench_routing_and_end_reveals_the_caret() {
    for &id in EXTRA_FIELDS {
        let mut app = field_app(id);
        set(&mut app, id, &"0123456789".repeat(50));
        app.text_input_state_mut(id).2.take();
        let mut terminal = Terminal::new(TestBackend::new(120, 60)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let area = app
            .text_input_area(id)
            .unwrap_or_else(|| panic!("{id:?} not rendered"));
        let initial = start(&app, id);
        let saved = state(&mut app, id);
        let focus = app.active_pane;
        for _ in 0..10 {
            wheel(
                &mut app,
                MouseEventKind::ScrollLeft,
                area.x + 1,
                area.y,
                KeyModifiers::SHIFT,
            );
        }
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert!(start(&app, id) < initial, "{id:?}");
        assert_eq!(state(&mut app, id), saved, "{id:?}");
        assert_eq!(app.active_pane, focus, "{id:?}");
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        );
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(start(&app, id), initial, "{id:?}");
        assert_eq!(state(&mut app, id), saved, "{id:?}");
    }
}
