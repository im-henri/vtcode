use crate::app::{App, PaletteCommand};
use crate::app_theme::{ThemeFile, theme_command_id};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{buffer::Buffer, style::{Color, Modifier, Style}};
use std::borrow::Cow;

const ORIGINAL: &str = "palette-original";
const LIGHT: &str = "palette-light";
const AMBER: &str = "palette-amber";

fn fixture() -> (tempfile::TempDir, App) {
    let workspace = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().to_path_buf());
    let base = crate::app_theme::load_builtin_themes().remove(0);
    app.loaded_themes = [
        (ORIGINAL, "Palette Original", "#101010"),
        (LIGHT, "Palette Light", "#eeeeee"),
        (AMBER, "Palette Amber", "#ffbb00"),
    ]
    .into_iter()
    .map(|(key, name, background)| {
        let mut theme = base.clone();
        theme.config_key = key.into();
        theme.syntax_theme = key.into();
        theme.name = name.into();
        theme
            .workbench_colors
            .insert("editor.background".into(), background.into());
        theme
    })
    .collect();
    app.settings_global.theme = Some(ORIGINAL.into());
    app.apply_effective_settings();
    assert_eq!(app.theme_key, ORIGINAL);
    (workspace, app)
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    assert!(!crate::input_keyboard::handle_keyboard_event(
        app,
        KeyEvent::new(code, modifiers)
    ));
}

fn open_themes(app: &mut App) {
    app.open_palette();
    app.palette.filtered = [ORIGINAL, LIGHT, AMBER]
        .map(|key| {
            app.palette
                .commands
                .iter()
                .position(|command| command.id == theme_command_id(key))
                .unwrap()
        })
        .to_vec();
    app.palette.selected = 0;
    app.preview_palette_theme();
}

fn filter(app: &mut App, query: &str) {
    app.palette.query = query.into();
    app.palette.cursor = query.len();
    app.palette.selection_anchor = None;
    app.update_palette_filter();
}

fn mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    render_palette(app);
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

fn render_palette(app: &mut App) {
    use ratatui::{Terminal, backend::TestBackend};
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| crate::ui_pickers::draw_command_palette(frame, app)).unwrap();
}

fn palette_row(app: &mut App, index: usize) -> ratatui::layout::Rect {
    render_palette(app);
    app.quick_picker.rows.iter().find(|(_, item)| *item == index).unwrap().0
}

#[test]
fn palette_theme_renders_input_and_match_colors_independently() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_workspace, mut app) = fixture();
    app.open_palette();
    filter(&mut app, "welco");
    let mut theme = app.loaded_themes.iter().find(|theme| theme.config_key == ORIGINAL).unwrap().clone();
    for (key, value) in [
        ("quickInput.background", "#0d1016"), ("input.background", "#1e2832"),
        ("input.foreground", "#b3b1ad"), ("quickInputList.focusBackground", "#263545"),
        ("quickInputList.focusHighlightForeground", "#e6b450"),
    ] {
        theme.workbench_colors.insert(key.into(), value.into());
    }
    app.colors = theme.to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| crate::ui_pickers::draw_command_palette(frame, &mut app)).unwrap();
    let cells = &terminal.backend().buffer().content;
    let query = cells.iter().find(|cell| cell.symbol() == "w").expect("palette query");
    assert_eq!(query.bg, Color::Rgb(30, 40, 50));
    assert_eq!(query.fg, Color::Rgb(179, 177, 173));
    let matched = cells.iter().find(|cell| cell.symbol() == "W").expect("Welcome result");
    assert_eq!(matched.fg, Color::Rgb(230, 180, 80));
    assert_eq!(matched.bg, app.colors.ui.bg_quick_selected);
}

#[test]
fn palette_theme_keyboard_navigation_previews_and_escape_restores() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    for (code, expected, background) in [
        (KeyCode::Down, LIGHT, Color::Rgb(238, 238, 238)),
        (KeyCode::Down, AMBER, Color::Rgb(255, 187, 0)),
        (KeyCode::Up, LIGHT, Color::Rgb(238, 238, 238)),
        (KeyCode::PageUp, ORIGINAL, Color::Rgb(16, 16, 16)),
        (KeyCode::PageDown, AMBER, Color::Rgb(255, 187, 0)),
    ] {
        key(&mut app, code, KeyModifiers::NONE);
        assert_eq!(app.theme_key, expected);
        assert_eq!(app.syntax_theme, expected);
        assert_eq!(app.colors.bg_editor, background);
        assert_eq!(app.current_settings().theme.as_deref(), Some(ORIGINAL));
        assert_eq!(app.build_session().theme.as_deref(), Some(ORIGINAL));
        assert!(app.palette.recent.is_empty());
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.palette.active);
    assert_eq!(app.theme_key, ORIGINAL);
    assert_eq!(app.colors.bg_editor, Color::Rgb(16, 16, 16));
    assert!(app.palette.theme_preview_original.is_none());
}

#[test]
fn palette_theme_confirmation_keeps_the_theme_and_next_cancel_restores_it() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(!app.palette.active);
    assert_eq!(app.theme_key, LIGHT);
    assert_eq!(app.current_settings().theme.as_deref(), Some(LIGHT));
    assert_eq!(app.build_session().theme.as_deref(), Some(LIGHT));
    assert_eq!(app.settings_global.theme.as_deref(), Some(LIGHT));
    assert_eq!(app.palette.recent, vec![theme_command_id(LIGHT)]);
    app.open_palette();
    filter(&mut app, "Preferences: Color Theme Palette Amber");
    assert_eq!(app.theme_key, AMBER);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.theme_key, LIGHT);
}

#[test]
fn palette_theme_filtering_restores_for_non_theme_empty_and_install_results() {
    let (_workspace, mut app) = fixture();
    app.open_palette();
    for query in [
        "zzzz-no-such-command-zzzz",
        "View: Toggle Minimap",
        "https://vscodethemes.com/e/test/theme",
    ] {
        filter(&mut app, "Preferences: Color Theme Palette Light");
        assert_eq!(app.theme_key, LIGHT);
        filter(&mut app, query);
        assert_eq!(app.theme_key, ORIGINAL, "query {query}");
        assert!(app.palette.theme_preview_original.is_none());
    }
}

#[test]
fn palette_theme_keyboard_toggles_and_outside_click_cancel() {
    for cancel in [KeyCode::F(1), KeyCode::Char('P')] {
        let (_workspace, mut app) = fixture();
        open_themes(&mut app);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        let modifiers = if cancel == KeyCode::F(1) {
            KeyModifiers::NONE
        } else {
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        };
        key(&mut app, cancel, modifiers);
        assert!(!app.palette.active);
        assert_eq!(app.theme_key, ORIGINAL);
    }
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
    assert!(!app.palette.active);
    assert_eq!(app.theme_key, ORIGINAL);
}

#[test]
fn palette_theme_mouse_hover_previews_only_visible_items_and_click_confirms() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    let light = palette_row(&mut app, 1);
    let amber = palette_row(&mut app, 2);
    mouse(&mut app, MouseEventKind::Moved, light.x, light.y);
    assert_eq!(app.theme_key, LIGHT);
    mouse(&mut app, MouseEventKind::Moved, amber.x, amber.y);
    assert_eq!(app.theme_key, AMBER);
    // A click can select a different item without a preceding hover event.
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), light.x, light.y);
    assert!(!app.palette.active);
    assert_eq!(app.theme_key, LIGHT);
    assert_eq!(app.current_settings().theme.as_deref(), Some(LIGHT));

    assert_eq!(app.settings_global.theme.as_deref(), Some(LIGHT));
    app.toggle_setting("minimap");
    assert_eq!(app.theme_key, LIGHT);

    open_themes(&mut app);
    let light_index = app.palette.filtered[1];
    app.palette.filtered.resize(30, light_index);
    let first = palette_row(&mut app, 0);
    mouse(&mut app, MouseEventKind::Moved, first.x, first.y);
    assert_eq!(app.theme_key, ORIGINAL);
    let below_list = app.quick_picker.list.bottom();
    mouse(
        &mut app,
        MouseEventKind::Moved,
        first.x,
        below_list,
    );
    assert_eq!(
        app.palette.selected, 0,
        "the bottom border is not a hidden theme row"
    );
    assert_eq!(app.theme_key, ORIGINAL);
    app.close_palette();
    assert_eq!(app.theme_key, LIGHT);
}

#[test]
fn palette_theme_preview_never_leaks_into_settings_saved_for_other_changes() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    app.show_minimap = !app.show_minimap;
    app.save_settings();
    assert_eq!(app.settings_effective.theme.as_deref(), Some(ORIGINAL));
    assert_eq!(app.settings_global.theme.as_deref(), Some(ORIGINAL));
    assert_eq!(
        app.theme_key, LIGHT,
        "saving other settings preserves the visible preview"
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    app.save_settings();
    assert_eq!(app.settings_global.theme.as_deref(), Some(LIGHT));
    assert_eq!(app.settings_effective.theme.as_deref(), Some(LIGHT));
}

#[test]
fn palette_theme_non_theme_confirmation_and_install_prompt_restore_original() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    let minimap = app.show_minimap;
    let index = app
        .palette
        .commands
        .iter()
        .position(|command| command.id == "toggle_minimap")
        .unwrap();
    app.palette.filtered.push(index);
    app.palette.selected = 3;
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.show_minimap, !minimap);
    assert_eq!(app.theme_key, ORIGINAL);
    open_themes(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    app.open_theme_install_prompt();
    assert_eq!(app.theme_key, ORIGINAL);
    assert!(app.theme_install_prompt_visible());
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.palette.active);
}

#[test]
fn palette_theme_catalog_refresh_preserves_the_selected_command() {
    let (_workspace, mut app) = fixture();
    app.open_palette();
    filter(&mut app, "Palette Light");
    assert_eq!(app.theme_key, LIGHT);
    let mut incoming: ThemeFile = app.loaded_themes[0].clone();
    incoming.config_key = "new-theme".into();
    incoming.name = "Palette Light Extra".into();
    app.loaded_themes.reverse(); // Force command indices to move during refresh.
    app.install_external_themes(vec![incoming]);
    assert_eq!(app.palette_selected_theme_key().as_deref(), Some(LIGHT));
    assert_eq!(app.theme_key, LIGHT);
    app.close_palette();
    assert_eq!(app.theme_key, ORIGINAL);
}

#[test]
fn palette_theme_unchanged_selection_reuses_syntax_theme_and_unknown_keys_restore() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    let (_, before) = app.highlighter.shared_sets();
    key(&mut app, KeyCode::Home, KeyModifiers::NONE);
    let light = palette_row(&mut app, 1);
    mouse(&mut app, MouseEventKind::Moved, light.x, light.y);
    let (_, after) = app.highlighter.shared_sets();
    assert!(std::sync::Arc::ptr_eq(&before, &after));

    let index = app.palette.commands.len();
    app.palette.commands.push(PaletteCommand {
        name: "Missing theme".into(),
        shortcut: String::new(),
        id: Cow::Borrowed("theme_missing"),
    });
    app.palette.filtered.push(index);
    app.palette.selected = 3;
    app.preview_palette_theme();
    assert_eq!(app.theme_key, ORIGINAL);
}

fn render_backdrop(app: &mut App, width: u16, height: u16) -> Buffer {
    use ratatui::{Terminal, backend::TestBackend};
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| {
        for cell in &mut frame.buffer_mut().content {
            cell.set_char('x').set_style(Style::default()
                .fg(app.colors.fg_text).bg(app.colors.bg_editor)
                .add_modifier(Modifier::ITALIC));
        }
        crate::ui_quick_picker::draw_active(frame, app);
        crate::ui_modal::draw_active(frame, app);
    }).unwrap();
    terminal.backend().buffer().clone()
}

fn assert_preview_backdrop(app: &App, buffer: &Buffer) {
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            assert!(!cell.modifier.contains(Modifier::DIM), "theme preview at {x},{y}");
            if app.quick_picker.area.contains((x, y).into()) {
                continue;
            }
            assert!(cell.modifier.contains(Modifier::ITALIC));
            assert_eq!(cell.symbol(), "x");
            assert_eq!(cell.fg, app.colors.fg_text);
            assert_eq!(cell.bg, app.colors.bg_editor);
        }
    }
}

#[test]
fn palette_theme_backdrop_previews_current_and_new_themes_across_resize() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    assert!(app.palette.theme_preview_original.is_none(), "current theme is also a preview");
    for selected in [0, 1, 2, 0] {
        app.palette.selected = selected;
        app.preview_palette_theme();
        for (width, height) in [(120, 40), (80, 24), (42, 14)] {
            let buffer = render_backdrop(&mut app, width, height);
            assert_preview_backdrop(&app, &buffer);
            if width == 120 {
                let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                assert!(text.contains("THEME PREVIEW"));
            }
        }
    }
}

#[test]
fn palette_theme_preview_updates_search_result_fills_and_cancel_restores_them() {
    use crate::{app::{GlobalSearchResult, SidebarView}, theme_workbench::ColorLayer};
    use ratatui::{Terminal, backend::TestBackend};
    let (_workspace, mut app) = fixture();
    let fills = [
        ColorLayer::rgba(128, 48, 0, 128),
        ColorLayer::rgba(0, 128, 64, 128),
        ColorLayer::rgba(48, 64, 255, 128),
    ];
    for (theme, fill) in app.loaded_themes.iter_mut()
        .zip(["#80300080", "#00804080", "#3040ff80"]) {
        theme.workbench_colors.insert("editor.findMatchHighlightBackground".into(), fill.into());
        theme.workbench_colors.insert("list.highlightForeground".into(), "#ffffff".into());
    }
    app.apply_effective_settings();
    app.sidebar_view = SidebarView::Search;
    app.global_search_query = "needle".into();
    app.global_search_results = vec![GlobalSearchResult {
        file_path: app.explorer.root.join("example.rs"), line_num: 1,
        line_text: "before needle after".into(), col_start: 7, col_end: 13,
        context_before: None, context_after: None,
    }].into();
    open_themes(&mut app);
    let render = |app: &mut App| {
        let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
        terminal.draw(|frame| crate::ui_search_panel::draw_search_panel(frame, app, frame.area())).unwrap();
        let buffer = terminal.backend().buffer();
        let (x, y) = crate::ui_control_visibility_tests::find_cell(buffer, "before needle after");
        (buffer[(x, y)].bg, buffer[(x + 7, y)].bg)
    };
    for selected in [0, 1, 2, 1] {
        app.palette.selected = selected;
        app.preview_palette_theme();
        let (row, matched) = render(&mut app);
        assert_eq!(matched, fills[selected].over(row).unwrap());
        assert_ne!(matched, row);
    }
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.theme_key, ORIGINAL);
    let (row, matched) = render(&mut app);
    assert_eq!(matched, fills[0].over(row).unwrap());
}

#[test]
fn palette_theme_backdrop_ends_for_other_commands_no_results_install_and_close() {
    let (_workspace, mut app) = fixture();
    app.open_palette();
    app.palette.filtered = [theme_command_id(LIGHT), "toggle_minimap".into()].iter().map(|id|
        app.palette.commands.iter().position(|command| command.id.as_ref() == id.as_str()).unwrap()).collect();
    app.palette.selected = 0;
    app.preview_palette_theme();
    render_backdrop(&mut app, 120, 40);
    let layout = (app.quick_picker.area, app.quick_picker.rows.clone());
    key(&mut app, KeyCode::Down, KeyModifiers::NONE);
    render_backdrop(&mut app, 120, 40);
    assert_eq!((app.quick_picker.area, app.quick_picker.rows.clone()), layout,
        "theme previews must not move the command rows under the pointer");
    for query in ["View: Toggle Minimap", "no-such-command-zzzz", "https://vscodethemes.com/e/test/theme"] {
        filter(&mut app, "Preferences: Color Theme Palette Light");
        let buffer = render_backdrop(&mut app, 120, 40);
        assert_preview_backdrop(&app, &buffer);
        filter(&mut app, query);
        let buffer = render_backdrop(&mut app, 120, 40);
        assert!(buffer[(119, 0)].modifier.contains(Modifier::DIM), "{query}");
        assert!(buffer[(0, 39)].modifier.contains(Modifier::DIM), "{query}");
    }
    app.close_palette();
    for confirm in [false, true] {
        app.open_palette();
        filter(&mut app, "Preferences: Color Theme Palette Light");
        key(&mut app, if confirm { KeyCode::Enter } else { KeyCode::Esc }, KeyModifiers::NONE);
        let buffer = render_backdrop(&mut app, 120, 40);
        assert!(buffer.content.iter().all(|cell| !cell.modifier.contains(Modifier::DIM)));
        assert_eq!(app.theme_key, if confirm { LIGHT } else { ORIGINAL });
    }
}

#[test]
fn palette_theme_backdrop_does_not_change_other_picker_kinds() {
    let (_workspace, mut app) = fixture();
    open_themes(&mut app);
    for kind in [crate::ui_quick_picker::Kind::Files, crate::ui_quick_picker::Kind::Symbols,
        crate::ui_quick_picker::Kind::Tasks, crate::ui_quick_picker::Kind::Branches,
        crate::ui_quick_picker::Kind::OpenWith] {
        use ratatui::{Terminal, backend::TestBackend};
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| crate::ui_quick_picker::draw(frame, &mut app, kind)).unwrap();
        assert!(terminal.backend().buffer()[(119, 0)].modifier.contains(Modifier::DIM), "{kind:?}");
    }
}

#[test]
fn palette_theme_backdrop_survives_cached_frames_and_clears_from_the_editor() {
    use crate::{animation_config::AnimationEffect, app::{ActivePane, Tab}, editor::Editor};
    use ratatui::{Terminal, backend::TestBackend};
    let workspace = tempfile::tempdir().unwrap();
    let mut app = App::new(workspace.path().into());
    let source = concat!(
        "// Color theme preview — syntax colors are shown at full intensity.\n",
        "use std::time::Duration;\n\n",
        "fn process_measurements(samples: &[f64], limit: f64) -> Vec<f64> {\n",
        "    let valid: Vec<_> = samples.iter().copied().filter(|value| value.is_finite()).collect();\n",
        "    let total = valid.iter().sum::<f64>();\n",
        "    println!(\"Processed {} samples, total = {:.2}\", valid.len(), total);\n",
        "    valid.into_iter().map(|value| (value / total).min(limit)).collect()\n",
        "}\n\n",
    ).repeat(6);
    let mut editor = Editor::test_with_text(&source);
    editor.file_path = Some(workspace.path().join("theme_preview.rs"));
    editor.cursor_line = 2;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_minimap = false;
    app.cursor_blinking = false;
    for effect in AnimationEffect::ALL { app.animation_settings.set(effect, effect == AnimationEffect::CursorSmear); }
    let mut terminal = Terminal::new(TestBackend::new(140, 48)).unwrap();
    for theme in ["monokai", "light", "high-contrast"] {
        assert!(app.set_theme_by_key(theme));
        app.open_palette();
        filter(&mut app, "Preferences: Color Theme");
        app.palette.selected = app.palette.filtered.iter().position(|&index|
            app.palette.commands[index].id == theme_command_id(theme)).unwrap();
        app.preview_palette_theme();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let expected = terminal.backend().buffer().clone();
        let text: String = expected.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Enter Apply   Esc Cancel"));
        let code = app.code_area.unwrap();
        assert!(!expected[(code.x, code.bottom() - 2)].modifier.contains(Modifier::DIM));
        assert!(!expected[(code.right() - 1, code.y)].modifier.contains(Modifier::DIM));
        for _ in 0..3 {
            app.rendering_animation_frame = true;
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            app.rendering_animation_frame = false;
            assert_eq!(*terminal.backend().buffer(), expected, "theme={theme}");
        }
        assert!(app.animation_frame_cache.replays > 0);
        if std::env::var_os("VTCODE_THEME_PREVIEW_SNAPSHOTS").is_some() {
            let rgb = |color| match color { Color::Rgb(r, g, b) => [r, g, b], _ => [128, 128, 128] };
            let cells: Vec<_> = expected.content.iter().map(|cell| serde_json::json!({
                "text": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg),
                "bold": cell.modifier.contains(Modifier::BOLD), "dim": cell.modifier.contains(Modifier::DIM),
            })).collect();
            std::fs::write(format!("target/theme-preview-{theme}.json"), serde_json::to_vec(&serde_json::json!({
                "width": expected.area.width, "height": expected.area.height, "cells": cells,
            })).unwrap()).unwrap();
        }
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert!(!buffer[(code.x, code.bottom() - 2)].modifier.contains(Modifier::DIM));
        assert_eq!(app.editor().unwrap().rope.to_string(), source);
    }
}
