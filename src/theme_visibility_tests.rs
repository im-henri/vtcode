use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};

#[test]
fn pane_separators_use_their_own_theme_roles_without_covering_editor_content() {
    for direction in [crate::app::SplitDirection::Horizontal, crate::app::SplitDirection::Vertical] {
        let mut app = App::test_new();
        app.settings_effective.workbench_color_customizations = serde_json::from_value(serde_json::json!({
            "editor.background":"#102030", "panel.background":"#304050",
            "panel.border":"#e0204080", "editorGroup.border":"#ff0000",
            "sideBySideEditor.verticalBorder":"#40e06080",
            "sideBySideEditor.horizontalBorder":"#4060e080",
            "activityBar.border":"#40c0e080",
            "sideBar.border":"#e0c04080", "sash.hoverBorder":"#00ffcc"
        })).unwrap();
        app.set_theme_by_key("dark");
        app.tabs = vec![crate::app::Tab::new_file(crate::editor::Editor::new()),
            crate::app::Tab::new_file(crate::editor::Editor::new())];
        app.create_split_group_dir(1, direction);
        app.show_panel = true;
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let panel = app.panel_area.unwrap();
        let body = app.workbench_body_area.unwrap();
        let rail = app.activity_bar_area.unwrap();
        let rail_border = &terminal.backend().buffer()[(rail.right() - 1, rail.y)];
        assert_eq!(rail_border.fg, crate::theme_workbench::ColorLayer::rgba(64, 192, 224, 128).over(rail_border.bg).unwrap());
        assert_eq!(body.bottom() + 1, panel.y);
        let border = &terminal.backend().buffer()[(panel.x, panel.y - 1)];
        assert_eq!(border.symbol(), "─");
        assert_eq!(border.fg, crate::theme_workbench::ColorLayer::rgba(224, 32, 64, 128).over(border.bg).unwrap());
        let (divider, _, _, _, _) = app.split_dividers.first().unwrap();
        let cell = &terminal.backend().buffer()[(divider.x, divider.y)];
        let layer = match direction {
            crate::app::SplitDirection::Horizontal => crate::theme_workbench::ColorLayer::rgba(64, 224, 96, 128),
            crate::app::SplitDirection::Vertical => crate::theme_workbench::ColorLayer::rgba(64, 96, 224, 128),
        };
        assert_eq!(cell.bg, app.colors.bg_editor);
        assert_eq!(cell.fg, layer.over(cell.bg).unwrap());
        snapshot(&terminal, &format!("pane-separators-{direction:?}"));
        app.mouse_col = panel.x;
        app.mouse_row = panel.y - 1;
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        assert_eq!(terminal.backend().buffer()[(panel.x, panel.y - 1)].fg, Color::Rgb(0, 255, 204));
    }
}

#[test]
fn transparent_panel_and_chat_borders_are_preserved() {
    let mut app = App::test_new();
    app.settings_effective.workbench_color_customizations = serde_json::from_value(serde_json::json!({
        "panel.border":"#ff000000", "sideBar.border":"#ff000000", "activityBar.border":"#ff000000"
    })).unwrap();
    app.set_theme_by_key("dark");
    app.show_panel = true;
    app.chat.visible = true;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    let mut terminal = Terminal::new(TestBackend::new(140, 34)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let panel = app.panel_area.unwrap();
    let rail = app.activity_bar_area.unwrap();
    let rail_border = &terminal.backend().buffer()[(rail.right() - 1, rail.y)];
    assert_eq!(rail_border.symbol(), " ");
    assert_eq!(rail_border.fg, rail_border.bg);
    let cell = &terminal.backend().buffer()[(panel.x, panel.y - 1)];
    assert_eq!(cell.symbol(), " ");
    assert_eq!(cell.fg, cell.bg);
    let chat = app.chat.area.unwrap();
    let cell = &terminal.backend().buffer()[(chat.x, chat.y + 3)];
    assert_eq!(cell.fg, cell.bg);
}

fn github_light() -> crate::app_theme::ThemeFile {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("light.json");
    // The relevant workbench colors from GitHub Light, including its omitted
    // editorCursor.background (which defaults to the light editor surface).
    std::fs::write(
        &path,
        r##"{"name":"GitHub Light","colors":{
        "editor.background":"#ffffff","editor.foreground":"#24292e",
        "foreground":"#444d56","descriptionForeground":"#6a737d",
        "focusBorder":"#2188ff","input.background":"#fafbfc",
        "input.foreground":"#2f363d","input.placeholderForeground":"#959da5",
        "quickInput.background":"#fafbfc","quickInput.foreground":"#2f363d",
        "list.activeSelectionBackground":"#e2e5e9","list.activeSelectionForeground":"#2f363d",
        "sideBar.background":"#f6f8fa","editorGroupHeader.tabsBackground":"#f6f8fa"
    }}"##,
    )
    .unwrap();
    crate::vscode_theme::load_theme_file(&path, None, Some("vs")).unwrap()
}

fn syntax_cyber() -> crate::app_theme::ThemeFile {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("syntax-cyber.json");
    // Relevant colors from the installed theme in the menu screenshot. Menu
    // colors are omitted, so the dropdown/list defaults must supply each pair.
    std::fs::write(
        &path,
        r##"{"name":"Syntax Cyber","colors":{
        "editor.background":"#000", "editor.foreground":"#fff",
        "sideBar.background":"#000", "foreground":"#000",
        "descriptionForeground":"#aaa", "contrastBorder":"#f0c05e",
        "dropdown.background":"#111", "dropdown.foreground":"#fff",
        "list.activeSelectionBackground":"#f0c05e88",
        "list.activeSelectionForeground":"#000"
    }}"##,
    )
    .unwrap();
    crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark")).unwrap()
}

fn assert_contrast(foreground: Color, background: Color, minimum: f32) {
    let ratio = crate::ui_colors::contrast_ratio(foreground, background).unwrap();
    assert!(
        ratio >= minimum,
        "{foreground:?} against {background:?}: contrast {ratio} < {minimum}"
    );
}

fn draw_palette(app: &mut App) -> Terminal<TestBackend> {
    app.text_input_regions.clear();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal
        .draw(|frame| crate::ui_pickers::draw_command_palette(frame, app))
        .unwrap();
    terminal
}

fn snapshot(terminal: &Terminal<TestBackend>, name: &str) {
    let Some(directory) = std::env::var_os("VTCODE_THEME_SNAPSHOTS") else {
        return;
    };
    let directory = std::path::Path::new(&directory);
    std::fs::create_dir_all(directory).unwrap();
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = buffer.content.iter().map(|cell| serde_json::json!({
        "text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg),
        "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
    })).collect();
    std::fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_vec(&serde_json::json!({
            "width": buffer.area.width, "height": buffer.area.height, "cells": cells,
        }))
        .unwrap(),
    )
    .unwrap();
}

fn assert_theme_text_contrast(foreground: Color, background: Color, colors: &crate::app_theme::ThemeColors) {
    let achievable = colors.ui.contrast_palette.into_iter().filter_map(|color| crate::ui_colors::contrast_ratio(color, background)).fold(0.0f32, f32::max);
    assert_contrast(foreground, background, 4.5f32.min(achievable));
}

#[test]
fn theme_visibility_palette_caret_survives_light_theme_mouse_and_keyboard_previews() {
    for mouse in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut app = App::new(temp.path().to_path_buf());
        let monokai = crate::app_theme::load_builtin_themes()
            .into_iter()
            .find(|theme| theme.config_key == "monokai")
            .unwrap();
        app.loaded_themes = vec![monokai, github_light()];
        app.set_theme_by_key("monokai");
        app.refresh_palette_commands();
        app.open_palette();
        app.palette.query = "theme".into();
        app.palette.cursor = 5;
        app.palette.filtered = ["monokai", "vscode-github-light"]
            .iter()
            .map(|key| {
                app.palette
                    .commands
                    .iter()
                    .position(|command| command.id == crate::app_theme::theme_command_id(key))
                    .unwrap()
            })
            .collect();
        app.palette.selected = 0;
        let initial = draw_palette(&mut app);
        snapshot(&initial, "monokai");
        if mouse {
            let row = app
                .quick_picker
                .rows
                .iter()
                .find(|(_, index)| *index == 1)
                .unwrap()
                .0;
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: row.x + 2,
                    row: row.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
        } else {
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            );
        }
        assert_eq!(app.theme_key, "vscode-github-light");
        assert!(
            crate::ui_colors::contrast_ratio(app.colors.fg_cursor, app.colors.bg_input).unwrap()
                < 1.1,
            "fixture must reproduce the original white cursor-color mix-up"
        );
        for position in [5, 2] {
            app.palette.cursor = position;
            let terminal = draw_palette(&mut app);
            snapshot(
                &terminal,
                if position == 5 {
                    "github-light"
                } else {
                    "github-light-caret-on-text"
                },
            );
            let cells = &terminal.backend().buffer().content;
            let query_start = cells
                .windows(5)
                .position(|cells| {
                    cells.iter().map(|cell| cell.symbol()).collect::<String>() == "theme"
                })
                .unwrap();
            let cell = &cells[query_start + position];
            assert_contrast(cell.bg, app.colors.bg_input, 3.0);
            assert_contrast(cell.fg, cell.bg, 4.5);
        }
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert_eq!(app.theme_key, "monokai");
    }
}

#[test]
fn native_shared_inputs_keep_carets_selections_and_text_readable() {
    for value in [20, 125, 250] {
        let background = Color::Rgb(value, value, value);
        let mut colors = github_light().to_theme_colors();
        colors.ui.preserve_theme_colors = false;
        let layer = crate::theme_workbench::ColorLayer::opaque(background);
        colors.ui.input_colors.foreground = layer;
        colors.ui.input_colors.background = layer;
        colors.ui.input_placeholder = layer;
        colors.ui.input_selection_background = layer;
        let mut terminal = Terminal::new(TestBackend::new(12, 2)).unwrap();
        terminal
            .draw(|frame| {
                crate::ui_widgets::InputField::new("abc").theme_colors(&colors)
                    .prefix("", background)
                    .cursor_marker("│")
                    .cursor_pos(1)
                    .selection_range(Some((0, 1)))
                    .render(frame, Rect::new(0, 0, 12, 1));
                crate::ui_widgets::InputField::new("").theme_colors(&colors)
                    .prefix("", background)
                    .placeholder_text("Search")
                    .render(frame, Rect::new(0, 1, 12, 1));
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_contrast(buffer[(1, 0)].bg, background, 3.0);
        for x in 0..3 {
            assert_theme_text_contrast(buffer[(x, 0)].fg, buffer[(x, 0)].bg, &colors);
        }
        assert_contrast(buffer[(0, 0)].bg, background, 1.5);
        assert_contrast(buffer[(0, 1)].fg, background, 3.0);
    }
}

#[test]
fn native_editor_caret_contrasts_with_highlighted_and_selected_cells() {
    for (word_wrap, large_file_mode) in [(false, false), (true, false), (false, true)] {
        for cursor_col in [1, 3] {
            let mut app = App::test_new();
            let mut editor = crate::editor::Editor::test_with_text("abc\n");
            editor.word_wrap = word_wrap;
            editor.large_file_mode = large_file_mode;
            editor.cursor_col = cursor_col;
            // A backwards selection includes the glyph under the caret.
            editor.selection_anchor = Some((0, 3));
            app.tabs.push(crate::app::Tab::new_file(editor));
            app.active_tab = 0;
            app.show_line_numbers = false;
            app.show_minimap = false;
            app.sticky_scroll = false;
            app.cursor_visible = true;
            let surface = Color::Rgb(240, 242, 244);
            app.colors.bg_line_highlight = surface;
            app.colors.bg_selection = surface;
            app.colors.bg_cursor = surface;
            app.colors.fg_cursor = surface;
            app.colors.ui.preserve_theme_colors = false;
            let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_editor::draw_editor(frame, &mut app, Rect::new(0, 0, 30, 4))
                })
                .unwrap();
            let area = app.code_area.unwrap();
            let buffer = terminal.backend().buffer();
            let caret = &buffer[(area.x + cursor_col as u16, area.y)];
            assert_eq!(caret.symbol(), if cursor_col == 1 { "b" } else { " " });
            let painted_surface = if cursor_col == 1 {
                buffer[(area.x + 2, area.y)].bg
            } else {
                surface
            };
            assert_contrast(caret.bg, painted_surface, 3.0);
            assert_contrast(caret.fg, caret.bg, 4.5);
        }
    }
}

#[test]
fn native_editor_selection_stands_out_from_the_current_line() {
    for (name, background, line, selection, foreground) in [
        // The screenshot theme's translucent #67769660 over #101010.
        (
            "subtle-blue",
            (16, 16, 16),
            (44, 49, 60),
            (49, 54, 66),
            (171, 178, 191),
        ),
        (
            "identical-dark",
            (24, 24, 24),
            (48, 48, 48),
            (48, 48, 48),
            (210, 210, 210),
        ),
        (
            "identical-light",
            (255, 255, 255),
            (238, 240, 242),
            (238, 240, 242),
            (36, 41, 46),
        ),
    ] {
        let rgb = |(r, g, b)| Color::Rgb(r, g, b);
        for (word_wrap, large_file_mode) in [(false, false), (true, false), (false, true)] {
            for multi_line in [false, true] {
                let mut app = App::test_new();
                let mut editor = crate::editor::Editor::test_with_text(
                    "alpha SELECTED omega\nbeta SECOND gamma\n",
                );
                editor.word_wrap = word_wrap;
                editor.word_wrap_column = 10;
                editor.large_file_mode = large_file_mode;
                editor.selection_anchor = Some((0, 6));
                editor.cursor_line = usize::from(multi_line);
                editor.cursor_col = if multi_line { 11 } else { 14 };
                app.tabs.push(crate::app::Tab::new_file(editor));
                app.active_tab = 0;
                app.show_line_numbers = false;
                app.show_minimap = false;
                app.show_indent_guides = false;
                app.sticky_scroll = false;
                app.cursor_visible = false;
                app.colors.bg_editor = rgb(background);
                app.colors.bg_line_highlight = rgb(line);
                app.colors.bg_selection = rgb(selection);
                app.colors.fg_text = rgb(foreground);
                app.colors.ui.preserve_theme_colors = false;
                let mut terminal = Terminal::new(TestBackend::new(32, 8)).unwrap();
                terminal
                    .draw(|frame| {
                        crate::ui_editor::draw_editor(frame, &mut app, Rect::new(0, 0, 32, 8));
                    })
                    .unwrap();
                if !word_wrap && !large_file_mode && !multi_line {
                    snapshot(&terminal, &format!("{name}-selection"));
                }
                let code = app.code_area.unwrap();
                let selected_rows = if word_wrap { 2 } else { 1 };
                let buffer = terminal.backend().buffer();
                let mut selected_cells = 0;
                for y in code.y..code.y + selected_rows {
                    for x in code.x..code.right() {
                        let cell = &buffer[(x, y)];
                        if cell.symbol().chars().any(|ch| ch.is_ascii_uppercase()) {
                            selected_cells += 1;
                            assert_contrast(cell.bg, app.colors.bg_line_highlight, 1.6);
                            assert_contrast(cell.bg, app.colors.bg_editor, 1.6);
                            assert_contrast(cell.fg, cell.bg, 4.5);
                        }
                    }
                }
                assert_eq!(
                    selected_cells, 8,
                    "{name}: selected text must remain visible"
                );
                assert_eq!(
                    buffer[(code.x, code.y)].bg,
                    if multi_line {
                        rgb(background)
                    } else {
                        rgb(line)
                    },
                    "only selected cells should receive the stronger background"
                );
            }
        }
    }
}

#[test]
fn theme_visibility_editor_selection_preserves_readable_theme_colors_over_search_matches() {
    for explicit_foreground in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selection.json");
        let mut theme = serde_json::json!({"name":"Readable Selection", "colors":{
            "editor.background":"#101010", "editor.foreground":"#eeeef2",
            "editor.lineHighlightBackground":"#303030",
            "editor.selectionBackground":"#405880",
            "editor.findMatchBackground":"#303030"
        }});
        if explicit_foreground {
            theme["colors"]["editor.selectionForeground"] = "#ffe080".into();
        }
        std::fs::write(&path, serde_json::to_vec(&theme).unwrap()).unwrap();
        let colors = crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark"))
            .unwrap()
            .to_theme_colors();
        for (word_wrap, large_file_mode) in [(false, false), (true, false), (false, true)] {
            let mut app = App::test_new();
            let mut editor = crate::editor::Editor::test_with_text("alpha SELECTED omega\n");
            editor.word_wrap = word_wrap;
            editor.large_file_mode = large_file_mode;
            editor.selection_anchor = Some((0, 6));
            editor.cursor_col = 14;
            app.tabs.push(crate::app::Tab::new_file(editor));
            app.active_tab = 0;
            app.show_line_numbers = false;
            app.show_minimap = false;
            app.sticky_scroll = false;
            app.cursor_visible = false;
            app.colors = colors;
            app.search_matches = vec![(0, 6, 8)];
            let mut terminal = Terminal::new(TestBackend::new(32, 4)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_editor::draw_editor(frame, &mut app, Rect::new(0, 0, 32, 4));
                })
                .unwrap();
            let code = app.code_area.unwrap();
            let buffer = terminal.backend().buffer();
            for col in 6..14 {
                let cell = &buffer[(code.x + col, code.y)];
                assert_eq!(cell.bg, Color::Rgb(64, 88, 128));
                assert_eq!(
                    cell.fg,
                    if explicit_foreground {
                        Color::Rgb(255, 224, 128)
                    } else {
                        Color::Rgb(238, 238, 242)
                    }
                );
            }
            assert_eq!(buffer[(code.x, code.y)].fg, colors.fg_text);
        }
    }
}

#[test]
fn native_terminal_caret_handles_colored_and_dimmed_cells() {
    let mut palette = github_light().to_theme_colors().ui;
    palette.preserve_theme_colors = false;
    let surface = Color::Rgb(240, 242, 244);
    palette.bg_terminal_cursor = surface;
    palette.fg_terminal_cursor = surface;
    for attributes in [
        "48;2;240;242;244",
        "2;48;2;240;242;244",
        "38;2;240;242;244;7",
    ] {
        let pty = crate::terminal::TerminalEmulator::test_with_output(
            12,
            2,
            &format!("\x1b[{attributes}mXY\r"),
        );
        let mut terminal = Terminal::new(TestBackend::new(12, 2)).unwrap();
        terminal
            .draw(|frame| {
                crate::ui_helpers::draw_terminal_emulator(
                    frame,
                    &pty,
                    Rect::new(0, 0, 12, 2),
                    &palette,
                    true,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let caret = &buffer[(0, 0)];
        assert_eq!(caret.symbol(), "X");
        assert_eq!(buffer[(1, 0)].bg, surface);
        assert_contrast(caret.bg, surface, 3.0);
        assert_contrast(caret.fg, caret.bg, 4.5);
        assert!(!caret.modifier.contains(ratatui::style::Modifier::DIM));
    }
}

#[test]
fn native_enabled_buttons_and_hovered_toggles_remain_readable() {
    let mut colors = github_light().to_theme_colors();
    colors.ui.preserve_theme_colors = false;
    let pale = Color::Rgb(250, 250, 250);
    colors.fg_selected = pale;
    colors.bg_selected = pale;
    colors.fg_error = pale;
    colors.ui.primary_button.foreground = crate::theme_workbench::ColorLayer::opaque(pale);
    colors.ui.primary_button.background = crate::theme_workbench::ColorLayer::opaque(pale);
    colors.ui.primary_button_hover = crate::theme_workbench::ColorLayer::opaque(pale);
    colors.ui.secondary_button.foreground = crate::theme_workbench::ColorLayer::opaque(pale);
    colors.ui.secondary_button.background = crate::theme_workbench::ColorLayer::opaque(pale);
    colors.ui.error_foreground = crate::theme_workbench::ColorLayer::opaque(pale);
    colors.bg_sc_button_secondary = pale;
    for focused in [false, true] {
        for destructive in [false, true] {
            let mut terminal = Terminal::new(TestBackend::new(16, 1)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_widgets::draw_dialog_button(
                        frame,
                        Rect::new(0, 0, 16, 1),
                        "Save",
                        &colors,
                        true,
                        focused,
                        destructive,
                        true,
                    )
                })
                .unwrap();
            let cell = &terminal.backend().buffer()[(2, 0)];
            assert_eq!(cell.symbol(), "S");
            assert_contrast(cell.fg, cell.bg, 4.5);
        }
    }
    for active in [false, true] {
        let style = crate::ui_widgets::toggle_button_style(active, true, pale, pale, pale, pale, &colors.ui.contrast_palette);
        assert_contrast(style.fg.unwrap(), style.bg.unwrap(), 4.5);
    }
}

#[test]
fn theme_visibility_preserves_already_readable_theme_colors() {
    let dark = Color::Rgb(30, 32, 34);
    let light = Color::Rgb(245, 247, 249);
    let caret = crate::ui_colors::block_cursor_style(dark, light, light, &[dark, light]);
    assert_eq!(caret.bg, Some(dark));
    assert_eq!(caret.fg, Some(light));
    let selected = crate::ui_colors::input_selection_style(light, dark, light, &[dark, light]);
    assert_eq!(selected.bg, Some(dark));
    assert_eq!(selected.fg, Some(light));
}

#[test]
fn native_source_control_actions_follow_hover_with_readable_colors() {
    let mut themes = crate::app_theme::load_builtin_themes();
    themes.extend([github_light(), syntax_cyber()]);
    for theme in themes {
        for flat_colors in [false, true] {
            let mut app = App::test_new();
            app.colors = theme.to_theme_colors();
            app.colors.ui.preserve_theme_colors = false;
            if flat_colors {
                // A theme may omit hover colors or give text the same color
                // as its button surface. Both actions must still be legible.
                app.colors.ui.primary_button_hover = app.colors.ui.primary_button.background;
                app.colors.ui.primary_button.foreground = app.colors.ui.primary_button.background;
                app.colors.ui.secondary_button_hover = app.colors.ui.secondary_button.background;
                app.colors.ui.secondary_button.foreground = app.colors.ui.secondary_button.background;
            }
            app.sidebar_view = crate::app::SidebarView::SourceControl;
            app.source_control_repository_state =
                crate::app::SourceControlRepositoryState::Available;
            app.show_explorer = true;
            app.source_control_input_mode = true;
            app.sc_amend_mode = true;
            app.source_control_commit_msg = "Improve button colors".into();
            app.sc_graph_collapsed = true;
            app.mouse_col = 60;
            app.mouse_row = 8;
            app.code_area = Some(Rect::new(40, 2, 40, 12));
            let area = Rect::new(2, 2, 36, 12);
            app.explorer_area = Some(area);
            let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
            let render = |terminal: &mut Terminal<TestBackend>, app: &mut App| {
                terminal
                    .draw(|frame| {
                        crate::ui_source_control::draw_source_control_sidebar(frame, app, area)
                    })
                    .unwrap();
            };
            let label = |terminal: &Terminal<TestBackend>, button: Rect| -> String {
                (button.x..button.right())
                    .map(|x| terminal.backend().buffer()[(x, button.y)].symbol())
                    .collect()
            };
            render(&mut terminal, &mut app);
            let buttons = [
                app.source_control_commit_area.unwrap(),
                app.source_control_amend_area.unwrap(),
            ];
            let resting: Vec<_> = buttons
                .iter()
                .map(|button| terminal.backend().buffer()[(button.x, button.y)].clone())
                .collect();
            for (index, button) in buttons.into_iter().enumerate() {
                assert!(
                    !label(&terminal, button).contains('['),
                    "typing is not button hover"
                );
                assert_theme_text_contrast(resting[index].fg, resting[index].bg, &app.colors);
                // Padding is part of the target, just like the label itself.
                crate::mouse::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind: MouseEventKind::Moved,
                        column: button.right() - 1,
                        row: button.y,
                        modifiers: KeyModifiers::NONE,
                    },
                );
                render(&mut terminal, &mut app);
                assert!(label(&terminal, button).contains('['));
                assert!(label(&terminal, button).contains(']'));
                let buffer = terminal.backend().buffer();
                for x in button.x..button.right() {
                    let cell = &buffer[(x, button.y)];
                    assert_theme_text_contrast(cell.fg, cell.bg, &app.colors);
                    assert_eq!(cell.bg, (if index == 0 { app.colors.ui.primary_button_hover }
                        else { app.colors.ui.secondary_button_hover })
                        .or(if index == 0 { app.colors.ui.primary_button.background } else { app.colors.ui.secondary_button.background })
                        .over(app.colors.bg_sidebar).unwrap_or(app.colors.bg_sidebar));
                }
                let other = buttons[1 - index];
                assert!(!label(&terminal, other).contains('['));
                assert_eq!(buffer[(other.x, other.y)].bg, resting[1 - index].bg);
                if !flat_colors {
                    snapshot(
                        &terminal,
                        &format!("sc-actions-{}-{index}", theme.config_key),
                    );
                }
                // Leaving for an empty editor must repaint the old highlight
                // even when no editor hover target changes.
                app.mouse_hover_changed = false;
                crate::mouse::handle_mouse(
                    &mut app,
                    MouseEvent {
                        kind: MouseEventKind::Moved,
                        column: 60,
                        row: 8,
                        modifiers: KeyModifiers::NONE,
                    },
                );
                assert!(app.mouse_hover_changed);
                render(&mut terminal, &mut app);
                assert!(!label(&terminal, button).contains('['));
                assert_eq!(
                    terminal.backend().buffer()[(button.x, button.y)].bg,
                    resting[index].bg
                );
            }
            app.bg_git_action_running = true;
            for button in buttons {
                app.mouse_col = button.x;
                app.mouse_row = button.y;
                render(&mut terminal, &mut app);
                assert!(
                    !label(&terminal, button).contains('['),
                    "busy actions must not look armed"
                );
                let disabled = &terminal.backend().buffer()[(button.x, button.y)];
                assert_contrast(disabled.fg, disabled.bg, 1.5);
                assert!(
                    crate::ui_colors::contrast_ratio(disabled.fg, disabled.bg)
                        < crate::ui_colors::contrast_ratio(resting[1].fg, resting[1].bg)
                );
            }
        }
    }
}

#[test]
fn theme_visibility_menu_enabled_selected_and_disabled_commands_are_distinct() {
    let mut themes = crate::app_theme::load_builtin_themes();
    themes.extend([github_light(), syntax_cyber()]);
    for theme in themes {
        let mut app = App::test_new();
        app.colors = theme.to_theme_colors();
        if theme.name == "Syntax Cyber" {
            assert_eq!(app.colors.bg_menu, Color::Rgb(17, 17, 17));
            assert_eq!(app.colors.fg_menu, Color::Rgb(255, 255, 255));
            assert_eq!(app.colors.bg_menu_selected, Color::Rgb(136, 110, 58));
            assert_eq!(app.colors.ui.fg_menu_selected, Color::Rgb(0, 0, 0));
        }
        app.menu_active = true;
        app.menu_open = true;
        app.menu_bar_area = Some(Rect::new(0, 0, 80, 1));
        let items = app.menu_items(0);
        let open = items
            .iter()
            .position(|(_, command)| *command == "change_dir")
            .unwrap();
        let save = items
            .iter()
            .position(|(_, command)| *command == "save")
            .unwrap();
        assert!(app.is_menu_cmd_disabled("save"));
        let x = crate::ui_chrome::menu_dropdown_anchor_x(&app) + 2;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.menu_item_selected = open - 1;
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );
        assert_eq!(app.menu_item_selected, open);
        terminal
            .draw(|frame| crate::ui_chrome::draw_menu_dropdown(frame, &app))
            .unwrap();
        if theme.name == "Syntax Cyber" {
            snapshot(&terminal, "syntax-cyber-file-menu");
        }
        let buffer = terminal.backend().buffer();
        let enabled = &buffer[(x, 2)];
        let selected = &buffer[(x, 2 + open as u16)];
        let disabled = &buffer[(x, 2 + save as u16)];
        assert_eq!(enabled.symbol(), "N");
        assert_eq!(selected.symbol(), "O");
        assert_eq!(disabled.symbol(), "S");
        assert_eq!(enabled.fg, app.colors.fg_menu);
        assert_eq!(selected.fg, app.colors.ui.fg_menu_selected);
        assert_eq!(selected.bg, app.colors.bg_menu_selected);
        assert!(selected.bg != enabled.bg || selected.modifier.contains(ratatui::style::Modifier::UNDERLINED),
            "{}: selection needs its registered fill or outline", theme.name);
        assert_eq!(disabled.bg, enabled.bg);
        assert_eq!(disabled.fg, app.colors.ui.fg_menu_disabled);
        let disabled_style = disabled.style();

        // A disabled row keeps its subdued style when the pointer enters it.
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: 2 + save as u16,
                modifiers: KeyModifiers::NONE,
            },
        );
        terminal
            .draw(|frame| crate::ui_chrome::draw_menu_dropdown(frame, &app))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(x, 2 + save as u16)].style(),
            disabled_style
        );
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: x,
                row: 2 + save as u16,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.menu_open, "disabled Save must not activate");
        assert!(app.tabs.is_empty());

        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: 2 + open as u16,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.menu_item_selected, open);
        terminal
            .draw(|frame| crate::ui_chrome::draw_menu_dropdown(frame, &app))
            .unwrap();
        let hovered = &terminal.backend().buffer()[(x, 2 + open as u16)];
        assert_eq!(hovered.fg, app.colors.ui.fg_menu_selected);
        assert_eq!(hovered.bg, app.colors.bg_menu_selected);
    }
}

#[test]
fn native_menu_shortcuts_icons_and_disabled_buttons_follow_their_state() {
    use crate::ui_widgets::{
        DropdownItem, DropdownMenu, draw_dialog_button, themed_dropdown_colors,
    };
    let mut colors = syntax_cyber().to_theme_colors();
    colors.ui.preserve_theme_colors = false;
    // Also exercise an inconsistent customization: disabled hints are brighter
    // than enabled text, and selected text is identical to its background.
    colors.fg_menu = Color::Rgb(140, 140, 140);
    colors.fg_dim = Color::Rgb(255, 255, 255);
    colors.ui.fg_menu_selected = colors.bg_menu_selected;
    colors.ui.secondary_button.foreground = crate::theme_workbench::ColorLayer::opaque(Color::Rgb(140, 140, 140));
    colors.ui.secondary_button.background = crate::theme_workbench::ColorLayer::opaque(Color::Rgb(17, 17, 17));
    colors.bg_sc_button_secondary = Color::Rgb(17, 17, 17);
    let items = [
        DropdownItem::new("Open")
            .shortcut("Ctrl+O")
            .icon("+", Color::Rgb(255, 255, 0)),
        DropdownItem::new("Save")
            .shortcut("Ctrl+S")
            .icon("+", Color::Rgb(255, 255, 0))
            .disabled(true),
    ];
    let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
    for selected in [0, 1] {
        let mut inner = Rect::default();
        terminal
            .draw(|frame| {
                inner = DropdownMenu::new(&items, themed_dropdown_colors(&colors))
                    .selected(selected)
                    .mouse(2, 3)
                    .min_width(24)
                    .render(frame);
                draw_dialog_button(
                    frame,
                    Rect::new(0, 5, 16, 1),
                    "Open",
                    &colors,
                    true,
                    false,
                    false,
                    true,
                );
                draw_dialog_button(
                    frame,
                    Rect::new(0, 6, 16, 1),
                    "Save",
                    &colors,
                    true,
                    true,
                    false,
                    false,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let disabled = &buffer[(inner.x + 2, inner.y + 1)];
        assert_eq!(disabled.symbol(), "S");
        assert_eq!(disabled.bg, colors.bg_menu);
        let normal = &buffer[(inner.x + 2, inner.y)];
        assert_contrast(normal.fg, normal.bg, 4.5);
        for x in inner.x..inner.x + inner.width {
            let cell = &buffer[(x, inner.y + 1)];
            if !cell.symbol().trim().is_empty() {
                assert_eq!(
                    cell.fg, disabled.fg,
                    "disabled icon/shortcut must also be subdued"
                );
            }
            if selected == 0 {
                let cell = &buffer[(x, inner.y)];
                if !cell.symbol().trim().is_empty() {
                    assert_eq!(
                        cell.fg, normal.fg,
                        "selected icon/shortcut must match the label"
                    );
                }
            }
        }
        let enabled_button = &buffer[(2, 5)];
        let disabled_button = &buffer[(2, 6)];
        assert_contrast(enabled_button.fg, enabled_button.bg, 4.5);
        assert_contrast(disabled_button.fg, disabled_button.bg, 1.5);
        assert!(
            crate::ui_colors::contrast_ratio(disabled_button.fg, disabled_button.bg).unwrap()
                < crate::ui_colors::contrast_ratio(enabled_button.fg, enabled_button.bg).unwrap()
                    * 0.8
        );
        assert!(
            !disabled_button
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }
}

#[test]
fn theme_visibility_menu_explicit_colors_override_dropdown_defaults_on_their_own_surfaces() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("menu.json");
    std::fs::write(
        &path,
        r##"{"colors":{
        "dropdown.background":"#000000", "dropdown.foreground":"#ffffff",
        "list.activeSelectionBackground":"#ff0000", "list.activeSelectionForeground":"#00ff00",
        "menu.background":"#102030", "menu.foreground":"#f0e0d0",
        "menu.selectionBackground":"#ffffff80", "menu.selectionForeground":"#00000080",
        "menu.separatorBackground":"#ff000080", "menu.border":"#abcdef",
        "disabledForeground":"#ffffff80", "button.secondaryBackground":"#000000"
    }}"##,
    )
    .unwrap();
    let colors = crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark"))
        .unwrap()
        .to_theme_colors();
    assert_eq!(colors.bg_menu, Color::Rgb(16, 32, 48));
    assert_eq!(colors.fg_menu, Color::Rgb(240, 224, 208));
    assert_eq!(colors.bg_menu_selected, Color::Rgb(136, 144, 152));
    assert_eq!(colors.ui.fg_menu_selected, Color::Rgb(68, 72, 76));
    assert_eq!(colors.ui.fg_menu_disabled, Color::Rgb(136, 144, 152));
    assert_eq!(colors.ui.disabled_foreground.over(colors.bg_sc_button_secondary), Some(Color::Rgb(128, 128, 128)));
    assert_eq!(colors.ui.fg_menu_separator, Color::Rgb(136, 16, 24));
    assert_eq!(colors.ui.border_menu, Color::Rgb(171, 205, 239));

    let items = [crate::ui_widgets::DropdownItem::new("Open")];
    let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_widgets::DropdownMenu::new(
                &items,
                crate::ui_widgets::themed_dropdown_colors(&colors),
            )
            .selected(usize::MAX)
            .render(frame);
        })
        .unwrap();
    let cell = &terminal.backend().buffer()[(2, 1)];
    assert_eq!(cell.symbol(), "O");
    assert_eq!(
        cell.fg, colors.fg_menu,
        "readable explicit colors must be preserved"
    );
    assert_eq!(cell.bg, colors.bg_menu);
}

#[test]
fn theme_visibility_status_bar_branch_and_hover_preserve_status_roles() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("status-bar.json");
    let mut themes = crate::app_theme::load_builtin_themes();
    for (name, colors) in [
        // Syntax Cyber's gold bar and black text reproduced the unreadable
        // branch: its dark hover background was painted even without a hover.
        (
            "Syntax Cyber",
            serde_json::json!({
                "editor.background": "#000", "list.hoverBackground": "#111",
                "statusBar.background": "#f0c05e", "statusBar.foreground": "#000",
                "statusBarItem.hoverBackground": "#111"
            }),
        ),
        (
            "Light status bar",
            serde_json::json!({
                "statusBar.background": "#f0f0f0", "statusBar.foreground": "#111",
                "list.hoverBackground": "#111"
            }),
        ),
        (
            "Dark status bar",
            serde_json::json!({
                "statusBar.background": "#111", "statusBar.foreground": "#eee",
                "statusBarItem.hoverBackground": "#111", "list.hoverBackground": "#eee"
            }),
        ),
    ] {
        std::fs::write(
            &path,
            serde_json::json!({"name": name, "colors": colors}).to_string(),
        )
        .unwrap();
        themes.push(crate::vscode_theme::load_theme_file(&path, None, None).unwrap());
    }

    let mut app = App::test_new();
    app.cached_git_branch = "master".into();
    let area = Rect::new(0, 1, 160, 1);
    app.tabs = vec![crate::app::Tab::new_file(crate::editor::Editor::test_with_text("status\n"))];
    app.active_tab = 0;
    app.status_bar_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(160, 2)).unwrap();
    for theme in themes {
        app.colors = theme.to_theme_colors();
        app.hovered_status_item = None;
        terminal
            .draw(|frame| crate::ui_panels::draw_status_bar(frame, &mut app, area))
            .unwrap();
        let branch = app.status_bar_items[0];
        assert_eq!(branch.2, "git_branch");
        let text: String = (branch.0..branch.0 + branch.1)
            .map(|x| terminal.backend().buffer()[(x, area.y)].symbol())
            .collect();
        assert_eq!(text.trim(), "⑂ master");
        for x in branch.0..branch.0 + branch.1 {
            let cell = &terminal.backend().buffer()[(x, area.y)];
            assert_eq!(
                cell.bg, app.colors.bg_statusbar,
                "{}: branch background", theme.name
            );
            assert_eq!(cell.fg, app.colors.fg_statusbar);
            // Authored status colors may intentionally be dim (e.g. Palenight).
            // Preserve them exactly; apply our readability floor to fallbacks.
            if !theme.workbench_colors.get("statusBar.foreground").is_some_and(serde_json::Value::is_string) {
                assert_contrast(cell.fg, cell.bg, 3.0);
            }
        }

        // Drive real mouse hover over the branch, diagnostics and cursor position.
        for index in 0..3 {
            let (x, _, _) = app.status_bar_items[index];
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: x,
                    row: area.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
            assert_eq!(app.hovered_status_item, Some(index));
            terminal
                .draw(|frame| crate::ui_panels::draw_status_bar(frame, &mut app, area))
                .unwrap();
            let cell = &terminal.backend().buffer()[(x, area.y)];
            assert_eq!(cell.bg, app.colors.bg_statusbar_item, "{}: hover fill", theme.name);
            assert_eq!(cell.fg, app.colors.ui.fg_statusbar_hover.unwrap_or(app.colors.fg_statusbar), "{}: hover text", theme.name);
            if theme.name == "Syntax Cyber" {
                assert_eq!(cell.bg, Color::Rgb(17, 17, 17));
            }
        }
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: 80,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_eq!(app.hovered_status_item, None);
        terminal
            .draw(|frame| crate::ui_panels::draw_status_bar(frame, &mut app, area))
            .unwrap();
        assert_eq!(
            terminal.backend().buffer()[(branch.0, area.y)].bg,
            app.colors.bg_statusbar
        );
    }
}

#[test]
fn theme_visibility_status_bar_preserves_explicit_hover_colors_and_gradients() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("status-bar.json");
    std::fs::write(&path, r##"{"colors": {
        "statusBar.background": "#e0c080", "statusBar.foreground": "#000000",
        "statusBarItem.hoverBackground": "#00000080", "statusBarItem.hoverForeground": "#ffffff"
    }}"##).unwrap();
    let mut app = App::test_new();
    app.colors = crate::vscode_theme::load_theme_file(&path, None, None)
        .unwrap()
        .to_theme_colors();
    app.cached_git_branch = "master".into();
    app.colors.gradients.bg_statusbar.end = Color::Rgb(255, 255, 255);
    let area = Rect::new(0, 0, 160, 1);
    let mut terminal = Terminal::new(TestBackend::new(160, 1)).unwrap();
    terminal
        .draw(|frame| crate::ui_panels::draw_status_bar(frame, &mut app, area))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(0, 0)].bg, app.colors.bg_statusbar);
    assert_ne!(terminal.backend().buffer()[(7, 0)].bg, app.colors.bg_statusbar);

    app.hovered_status_item = Some(0);
    terminal
        .draw(|frame| crate::ui_panels::draw_status_bar(frame, &mut app, area))
        .unwrap();
    let (x, width, _) = app.status_bar_items[0];
    for x in x..x + width {
        let cell = &terminal.backend().buffer()[(x, 0)];
        assert_eq!(cell.bg, Color::Rgb(112, 96, 64));
        assert_eq!(cell.fg, Color::Rgb(255, 255, 255));
        assert_contrast(cell.fg, cell.bg, 4.5);
    }
}
