use crate::{app::App, ui_settings::draw_settings_editor};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

fn app() -> App {
    let mut app = App::test_new();
    app.open_settings_tab();
    app.rebuild_keybinding_settings_cache();
    app
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    app.text_input_regions.clear();
    app.settings_scroll_areas.clear();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw_settings_editor(frame, app, frame.area()))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    buffer
        .content
        .chunks(buffer.area.width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn click(app: &mut App, column: u16, row: u16) {
    mouse(app, MouseEventKind::Down(MouseButton::Left), column, row);
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_dialogs::handle_settings_editor_key(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn capture(buffer: &Buffer, name: &str) {
    let Some(folder) = std::env::var_os("VTCODE_SETTINGS_CAPTURE") else {
        return;
    };
    let folder = std::path::PathBuf::from(folder);
    std::fs::create_dir_all(&folder).unwrap();
    let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
        "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
        "bold": cell.modifier.contains(Modifier::BOLD),
        "underline": cell.modifier.contains(Modifier::UNDERLINED),
        "underline_color": format!("{:?}", cell.underline_color)
    })).collect();
    std::fs::write(
        folder.join(format!("settings-{name}.json")),
        serde_json::to_vec(&serde_json::json!({
            "width": buffer.area.width, "height": buffer.area.height, "cells": cells
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn vtcomplete_introduction_wraps_above_settings_and_stays_on_its_own_page() {
    for light in [false, true] {
        let mut app = app();
        if light {
            app.colors = crate::app_theme::EditorTheme::Light.colors();
        }
        app.select_settings_page(1, Some("VTComplete"));
        for (width, height) in [(110, 32), (46, 18)] {
            let buffer = render(&mut app, width, height);
            let content = app.settings_sidebar_area.unwrap();
            let header = buffer.content[..content.y as usize * width as usize]
                .chunks(width as usize)
                .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
                .collect::<Vec<_>>()
                .join(" ");
            let header = header.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(header.contains("VTCode's custom autocomplete engine. No language server required."));
            assert!(content.height >= 3);
            assert!(text(&buffer).contains("Minimum Prefix Characters"));
            capture(&buffer, &format!("vtcomplete-{width}-light-{light}"));
        }
        app.select_settings_page(1, Some("Suggestions"));
        assert!(!text(&render(&mut app, 110, 32)).contains("custom autocomplete engine"));
    }
}

#[test]
fn settings_subpages_cover_every_setting_without_empty_pages() {
    let mut app = app();
    let items = app.settings_items();
    for (category, section, _) in App::settings_page_options() {
        app.select_settings_page(category, section);
        let filtered = app.settings_filtered_indices_for_category(category);
        assert!(!filtered.is_empty(), "empty {category}/{section:?}");
        assert!(filtered.contains(&app.settings_selected));
        if let Some(section) = section {
            assert!(filtered.iter().all(|&i| !items[i].2.starts_with("heading_")
                && App::settings_section_for_id(&items[i].2) == Some(section)));
        }
    }
    for (_, _, id) in &items {
        let category = App::settings_category_for_id(id);
        if !id.starts_with("heading_") && !App::settings_sections(category).is_empty() {
            assert!(
                App::settings_sections(category)
                    .contains(&App::settings_section_for_id(id).unwrap()),
                "missing page for {id}"
            );
        }
    }
}

#[test]
fn settings_search_crosses_subpages_and_clearing_restores_the_page() {
    let mut app = app();
    app.select_settings_page(1, Some("Cursor"));
    app.settings_editor_search = "auto save".into();
    app.settings_editor_search_cursor = 9;
    let rendered = render(&mut app, 120, 36);
    assert!(text(&rendered).contains("Files: Auto Save"));
    assert!(!text(&rendered).contains("Cursor Style"));
    let clear = app.settings_clear_search_area.unwrap();
    click(&mut app, clear.x, clear.y);
    let rendered = render(&mut app, 120, 36);
    assert!(text(&rendered).contains("Cursor Style"));
    assert!(!text(&rendered).contains("Files: Auto Save"));
    assert_eq!(app.settings_selected_section(1), Some("Cursor"));
}

#[test]
fn settings_tree_clicks_open_real_subpages_and_leave_search_mode() {
    let mut app = app();
    app.settings_category_collapsed[1] = false;
    app.settings_category_collapsed[2] = false;
    app.settings_editor_search = "nothing matches".into();
    render(&mut app, 120, 38);
    let area = app.settings_category_area.unwrap();
    let row = app
        .settings_category_visual_rows()
        .iter()
        .position(|entry| *entry == (1, true, "Minimap"))
        .unwrap();
    click(&mut app, area.x + 6, area.y + row as u16);
    assert_eq!(app.settings_selected_section(1), Some("Minimap"));
    assert!(app.settings_editor_search.is_empty());
    assert!(
        !app.settings_category_collapsed[2],
        "other branches stay expanded"
    );
    let rendered = text(&render(&mut app, 120, 38));
    assert!(rendered.contains("Show Minimap"));
    assert!(rendered.contains("Minimap Width"));
    assert!(!rendered.contains("Tab Size"));
}

#[test]
fn settings_tree_keyboard_expands_navigates_collapses_and_reveals_selection() {
    let mut app = app();
    app.settings_focus_categories = true;
    render(&mut app, 120, 20);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.settings_category_selected, 1);
    key(&mut app, KeyCode::Right);
    assert!(!app.settings_category_collapsed[1]);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.settings_selected_section(1), Some("Editing"));
    key(&mut app, KeyCode::Down);
    assert_eq!(app.settings_selected_section(1), Some("Cursor"));
    key(&mut app, KeyCode::Right);
    assert_eq!(app.settings_selected_section(1), Some("Cursor"));
    key(&mut app, KeyCode::Left);
    assert_eq!(app.settings_selected_section(1), None);
    assert!(!app.settings_category_collapsed[1]);
    key(&mut app, KeyCode::Left);
    assert!(app.settings_category_collapsed[1]);
    app.settings_category_collapsed.fill(false);
    key(&mut app, KeyCode::End);
    let last_category = App::settings_categories().len() - 1;
    assert_eq!(app.settings_category_selected, last_category);
    assert!(app.settings_category_scroll > 0);
    let rendered = text(&render(&mut app, 120, 20));
    assert!(rendered.contains(App::settings_categories()[last_category]));
    key(&mut app, KeyCode::Home);
    assert_eq!(app.settings_category_scroll, 0);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    assert!(app.settings_focus_control);
    assert!(!app.settings_focus_categories);
}

#[test]
fn settings_tree_chevrons_preserve_the_current_subpage_and_global_search() {
    let mut app = app();
    app.select_settings_page(1, Some("Minimap"));
    render(&mut app, 120, 36);
    let area = app.settings_category_area.unwrap();
    click(&mut app, area.x + 2, area.y + 1);
    assert!(app.settings_category_collapsed[1]);
    assert_eq!(app.settings_selected_section(1), Some("Minimap"));
    assert!(text(&render(&mut app, 120, 36)).contains("Minimap Width"));
    key(&mut app, KeyCode::Right);
    assert!(!app.settings_category_collapsed[1]);
    assert_eq!(app.settings_selected_section(1), Some("Minimap"));

    app.settings_editor_search = "auto save".into();
    app.settings_focus_categories = false;
    render(&mut app, 120, 36);
    let row = app
        .settings_category_visual_rows()
        .iter()
        .position(|entry| *entry == (2, false, "Workbench"))
        .unwrap();
    click(&mut app, area.x + 2, area.y + row as u16);
    assert!(!app.settings_category_collapsed[2]);
    assert_eq!(app.settings_editor_search, "auto save");
    assert_eq!(app.settings_selected_section(1), Some("Minimap"));
}

#[test]
fn settings_navigation_keyboard_focus_remains_visible_during_global_search() {
    let mut app = app();
    app.select_settings_page(1, Some("Cursor"));
    app.settings_editor_search = "auto save".into();
    render(&mut app, 120, 36);
    key(&mut app, KeyCode::Tab);
    let buffer = render(&mut app, 120, 36);
    let area = app.settings_category_area.unwrap();
    let index = app
        .settings_category_visual_rows()
        .iter()
        .position(|entry| *entry == (1, true, "Cursor"))
        .unwrap();
    let row = area.y + (index - app.settings_category_scroll) as u16;
    assert!(
        buffer[(area.x + 6, row)]
            .modifier
            .contains(Modifier::UNDERLINED)
    );
    assert!(text(&buffer).contains("Search Results"));
    assert_eq!(app.settings_editor_search, "auto save");
}

#[test]
fn settings_tree_wheel_and_drag_are_independent_of_the_setting_list() {
    let mut app = app();
    app.settings_category_collapsed.fill(false);
    render(&mut app, 120, 20);
    let area = app.settings_category_area.unwrap();
    let content_scroll = app.settings_editor_scroll;
    mouse(&mut app, MouseEventKind::ScrollDown, area.x + 6, area.y + 3);
    assert_eq!(app.settings_category_scroll, 3);
    assert_eq!(app.settings_editor_scroll, content_scroll);
    render(&mut app, 120, 20);
    assert_eq!(
        app.settings_category_scroll, 3,
        "drawing must not undo wheel scrolling"
    );
    click(&mut app, area.right() - 1, area.bottom() - 1);
    assert!(app.dragging_settings_categories);
    assert!(app.settings_category_scroll > 3);
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        area.right() - 1,
        area.y,
    );
    assert_eq!(app.settings_category_scroll, 0);
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        area.right() - 1,
        area.y,
    );
    assert!(!app.dragging_settings_categories);
    assert_eq!(app.settings_editor_scroll, content_scroll);
}

#[test]
fn settings_expanded_tree_clamps_scroll_after_collapse_and_resize() {
    let mut app = app();
    app.settings_category_collapsed.fill(false);
    app.settings_category_scroll = usize::MAX;
    render(&mut app, 120, 20);
    assert!(app.settings_category_scroll < App::settings_page_options().len());
    app.settings_category_collapsed.fill(true);
    render(&mut app, 120, 36);
    assert_eq!(app.settings_category_scroll, 0);
    assert!(app.settings_category_scrollbar().is_none());
    render(&mut app, 40, 24);
    assert!(app.settings_category_area.is_none());
    assert!(app.settings_category_picker_area.is_some());
}

#[test]
fn settings_wide_spacing_does_not_expand_dropdown_click_targets() {
    let mut app = app();
    app.select_settings_page(2, Some("Appearance"));
    let buffer = render(&mut app, 140, 38);
    let area = app.settings_sidebar_area.unwrap();
    let value_row = buffer
        .content
        .chunks(buffer.area.width as usize)
        .position(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .contains("Dark+")
        })
        .unwrap() as u16;
    assert!(value_row > area.y + 2);
    click(&mut app, area.x + 3, value_row - 1);
    assert!(
        !app.settings_dropdown_open,
        "space above input is not an input"
    );
    let focused = render(&mut app, 140, 38);
    assert_eq!(
        focused[(area.x + 6, value_row - 1)].bg,
        app.colors.ui.bg_settings_focus,
        "the input's surrounding space keeps the same focus background"
    );
    click(&mut app, area.x + 3, value_row);
    assert!(app.settings_dropdown_open);
    assert_eq!(app.settings_dropdown_setting_id.as_deref(), Some("theme"));
}

#[test]
fn settings_pages_render_hierarchy_controls_and_scopes_in_light_and_dark() {
    for light in [false, true] {
        let mut app = app();
        if light {
            app.colors = crate::app_theme::EditorTheme::Light.colors();
        }
        app.settings_category_collapsed[1] = false;
        app.settings_category_collapsed[2] = false;
        for (category, section) in [
            (2, "Appearance"),
            (1, "Minimap"),
            (1, "Formatting"),
            (4, "Saving"),
        ] {
            app.select_settings_page(category, Some(section));
            let buffer = render(&mut app, 130, 40);
            let rendered = text(&buffer);
            assert!(rendered.contains(App::settings_categories()[category]));
            assert!(rendered.contains(section));
            assert!(rendered.contains("Changes save automatically"));
            assert!(rendered.contains("Open Settings (JSON)"));
            capture(&buffer, &format!("{section}-light-{light}"));
        }
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.select_settings_page(1, Some("Cursor"));
        for width in [40, 70, 100] {
            let buffer = render(&mut app, width, 30);
            let bounds = buffer.area;
            for area in [
                app.settings_sidebar_area,
                app.settings_category_area,
                app.settings_category_picker_area,
                app.settings_scope_areas[0],
                app.settings_scope_areas[1],
            ]
            .into_iter()
            .flatten()
            {
                assert_eq!(area.intersection(bounds), area);
            }
            assert!(text(&buffer).contains("Cursor Style"));
            capture(&buffer, &format!("compact-{width}-light-{light}"));
        }
    }
}
