use crate::{app::App, ui_settings::draw_settings_editor};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

fn fixture() -> App {
    let mut app = App::test_new();
    app.open_settings_tab();
    app.cursor_visible = false;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    app.text_input_regions.clear();
    app.settings_scroll_areas.clear();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| draw_settings_editor(f, app, f.area()))
        .unwrap()
        .buffer
        .clone()
}

fn row(buffer: &Buffer, area: Rect, y: u16) -> String {
    (area.x..area.right().saturating_sub(1))
        .map(|x| buffer[(x, y)].symbol())
        .collect()
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
fn settings_wheel_moves_rendered_content_one_line_and_keeps_headers_fixed() {
    for width in [40, 80, 140] {
        let mut app = fixture();
        app.scroll_speed = 1;
        let before = draw(&mut app, width, 24);
        let area = app.settings_sidebar_area.unwrap();
        let layout = app.settings_line_layout(area.width.saturating_sub(3) as usize);
        assert!(layout.max_scroll(area.height as usize) > 2);
        mouse(&mut app, MouseEventKind::ScrollDown, area.x + 3, area.y + 1);
        let after = draw(&mut app, width, 24);
        assert_eq!(app.settings_editor_scroll, 1);
        assert!(
            layout.row_at(1).unwrap().start < 1,
            "can stop inside the first setting"
        );
        for y in area.y..area.bottom() - 1 {
            assert_eq!(
                row(&after, area, y),
                row(&before, area, y + 1),
                "width {width}, row {y}"
            );
        }
        for y in 0..area.y {
            assert_eq!(
                row(&after, before.area, y),
                row(&before, before.area, y),
                "fixed header {y}"
            );
        }
        mouse(&mut app, MouseEventKind::ScrollUp, area.x + 3, area.y + 1);
        draw(&mut app, width, 24);
        assert_eq!(app.settings_editor_scroll, 0);
    }
}

#[test]
fn single_tall_setting_scrolls_without_skipping_its_description_or_control() {
    let mut app = fixture();
    app.settings_editor_search = "Tab Size".into();
    draw(&mut app, 24, 10);
    let area = app.settings_sidebar_area.unwrap();
    let layout = app.settings_line_layout(area.width.saturating_sub(3) as usize);
    assert_eq!(layout.rows.len(), 1);
    assert!(layout.total > area.height as usize);
    assert!(crate::mouse::settings_scrollbar_metrics(&app, area).is_some());
    let max = layout.max_scroll(area.height as usize);
    for line in 0..=max {
        app.settings_editor_scroll = line;
        draw(&mut app, 24, 10);
        assert_eq!(app.settings_editor_scroll, line);
    }
    let control = layout.rows[0].control;
    assert!(control >= max && control < max + area.height as usize);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 3,
        area.y + (control - max) as u16,
    );
    assert!(app.settings_value_editing);
    assert_eq!(app.settings_value_setting_id.as_deref(), Some("tab_size"));
}

#[test]
fn partially_visible_checkboxes_numeric_inputs_and_dropdowns_keep_correct_hit_areas() {
    for id in ["word_wrap", "tab_size", "theme"] {
        let mut app = fixture();
        draw(&mut app, 40, 18);
        let area = app.settings_sidebar_area.unwrap();
        let layout = app.settings_line_layout(area.width.saturating_sub(3) as usize);
        let items = app.settings_items();
        let index = items.iter().position(|(_, _, key)| key == id).unwrap();
        let setting = layout.row_for_item(index).unwrap();
        app.settings_editor_scroll = setting.start + 1;
        let scroll = app.settings_editor_scroll;
        let control = setting.start + setting.control;
        draw(&mut app, 40, 18);
        assert_eq!(app.settings_editor_scroll, scroll);
        let before = app.default_word_wrap;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            area.x + 3,
            area.y + (control - scroll) as u16,
        );
        assert_eq!(app.settings_selected, index);
        assert_eq!(
            app.settings_editor_scroll, scroll,
            "clicking a visible control must not snap to its title"
        );
        match id {
            "word_wrap" => assert_ne!(app.default_word_wrap, before),
            "tab_size" => {
                assert!(app.settings_value_editing);
                draw(&mut app, 40, 18);
                let input = app
                    .text_input_area(crate::text_input::TextInputId::SettingsValue)
                    .unwrap();
                assert_eq!(input.y, area.y + (control - scroll) as u16);
            }
            _ => {
                assert!(app.settings_dropdown_open);
                assert_eq!(app.settings_dropdown_setting_id.as_deref(), Some("theme"));
            }
        }
    }
}

#[test]
fn settings_scrollbar_has_stable_geometry_and_grabbing_it_does_not_jump() {
    let mut app = fixture();
    draw(&mut app, 100, 24);
    let area = app.settings_sidebar_area.unwrap();
    let metrics = crate::mouse::settings_scrollbar_metrics(&app, area).unwrap();
    for line in 0..=metrics.3 {
        app.settings_editor_scroll = line;
        assert_eq!(
            crate::mouse::settings_scrollbar_metrics(&app, area),
            Some(metrics)
        );
    }
    app.settings_editor_scroll = 2;
    draw(&mut app, 100, 24);
    let (x, track, thumb, max) = metrics;
    let y = area.y + (2 * (track - thumb) / max) as u16;
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, y);
    assert_eq!(app.settings_editor_scroll, 2);
    assert!(app.settings_scrollbar_drag.is_some());
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, y);
    assert_eq!(
        app.settings_editor_scroll, 2,
        "horizontal motion cannot move the thumb"
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        0,
        u16::MAX,
    );
    assert_eq!(app.settings_editor_scroll, max);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 0);
    assert_eq!(app.settings_editor_scroll, 0);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
    assert!(app.settings_scrollbar_drag.is_none());
}

#[test]
fn settings_keyboard_reveal_and_search_resize_clamping_use_line_bounds() {
    let mut app = fixture();
    draw(&mut app, 40, 18);
    let area = app.settings_sidebar_area.unwrap();
    let layout = app.settings_line_layout(area.width.saturating_sub(3) as usize);
    app.settings_selected = layout.rows.last().unwrap().index;
    app.settings_focus_categories = false;
    app.settings_focus_control = true;
    app.ensure_settings_selection_visible();
    for _ in 0..layout.rows.len() {
        draw(&mut app, 40, 18);
        let setting = layout.row_for_item(app.settings_selected).unwrap();
        let control = setting.start + setting.control;
        assert!(
            (app.settings_editor_scroll..app.settings_editor_scroll + area.height as usize)
                .contains(&control)
        );
        crate::input_dialogs::handle_settings_editor_key(
            &mut app,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );
    }
    app.settings_editor_scroll = usize::MAX;
    draw(&mut app, 140, 40);
    let area = app.settings_sidebar_area.unwrap();
    let layout = app.settings_line_layout(area.width.saturating_sub(3) as usize);
    assert_eq!(
        app.settings_editor_scroll,
        layout.max_scroll(area.height as usize)
    );
    app.settings_editor_search = "there_are_no_settings_matching_this".into();
    draw(&mut app, 140, 40);
    assert_eq!(app.settings_editor_scroll, 0);
    assert!(
        crate::mouse::settings_scrollbar_metrics(&app, app.settings_sidebar_area.unwrap())
            .is_none()
    );
}

#[test]
fn settings_layout_cache_reuses_value_edits_and_reflows_labels_filters_and_widths() {
    use std::rc::Rc;
    let app = fixture();
    let mut items = app.settings_items();
    let filtered = app.settings_filtered_indices_for_category(0);
    let narrow = app.settings_layout_cache.get(&items, &filtered, 16);
    items[filtered[0]].1 = "changed value".into();
    assert!(Rc::ptr_eq(
        &narrow,
        &app.settings_layout_cache.get(&items, &filtered, 16)
    ));
    let wide = app.settings_layout_cache.get(&items, &filtered, 80);
    assert!(wide.total < narrow.total);
    assert!(!Rc::ptr_eq(&narrow, &wide));
    items[filtered[0]].0 = "Changed label".into();
    let changed = app.settings_layout_cache.get(&items, &filtered, 16);
    assert!(!Rc::ptr_eq(&narrow, &changed));
    assert_eq!(changed.rows[0].lines[0], "Changed label");
    let one = app.settings_layout_cache.get(&items, &filtered[..1], 16);
    assert_eq!(one.rows.len(), 1);
    assert!(one.total < changed.total);
    assert!(
        one.row_at(one.total).is_none(),
        "blank space below content is not a setting"
    );
}
