use crate::{app::App, app_theme::ThemeFile, ui_colors::contrast_ratio};
use ratatui::{
    Terminal,
    backend::TestBackend,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Paragraph,
};

fn theme(colors: serde_json::Value, kind: &str) -> ThemeFile {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("theme.json");
    std::fs::write(
        &file,
        serde_json::json!({"name":"Audit", "colors":colors}).to_string(),
    )
    .unwrap();
    crate::vscode_theme::load_theme_file(&file, None, Some(kind)).unwrap()
}

#[test]
fn translucent_foregrounds_follow_independently_customized_surfaces() {
    let palette = theme(serde_json::json!({
        "editor.background":"#050505", "sideBar.background":"#111111",
        "input.background":"#333333", "settings.checkboxBackground":"#202020",
        "settings.checkboxForeground":"#ffffff80", "settings.dropdownBackground":"#404040",
        "settings.dropdownForeground":"#ffffff80", "settings.numberInputBackground":"#606060",
        "settings.numberInputForeground":"#ffffff80", "settings.textInputBackground":"#808080",
        "settings.textInputForeground":"#ffffff80", "activityBarBadge.background":"#a0a0a0",
        "activityBarBadge.foreground":"#ffffff80", "quickInputList.focusBackground":"#c0c0c0",
        "quickInputList.focusForeground":"#ffffff80", "editorSuggestWidget.selectedBackground":"#e0e0e0",
        "editorSuggestWidget.selectedForeground":"#ffffff80", "tab.hoverBackground":"#202020",
        "tab.hoverForeground":"#ffffff80", "tab.unfocusedActiveBackground":"#404040",
        "tab.unfocusedActiveForeground":"#ffffff80", "editorInlayHint.background":"#606060",
        "editorInlayHint.foreground":"#ffffff80", "breadcrumb.background":"#808080",
        "breadcrumb.foreground":"#ffffff80", "titleBar.activeBackground":"#a0a0a0",
        "titleBar.activeForeground":"#ffffff80", "terminal.selectionBackground":"#c0c0c0",
        "terminal.selectionForeground":"#ffffff80", "list.activeSelectionBackground":"#e0e0e0",
        "list.activeSelectionForeground":"#ffffff80", "sideBarSectionHeader.background":"#202020",
        "sideBarSectionHeader.foreground":"#ffffff80", "editorOverviewRuler.background":"#404040",
        "editorOverviewRuler.errorForeground":"#ffffff80", "minimap.background":"#606060",
        "minimap.errorHighlight":"#ffffff80", "editorGutter.background":"#808080",
        "editorLineNumber.foreground":"#ffffff80"
    }), "vs-dark").to_theme_colors();
    for (foreground, expected) in [
        (palette.ui.fg_activity_badge, 208),
        (palette.ui.fg_quick_selected, 224),
        (palette.ui.suggest_selection.foreground.over(Color::Rgb(224, 224, 224)).unwrap(), 240),
        (palette.ui.fg_tab_hover, 144),
        (palette.ui.fg_unfocused_tab, 160),
        (palette.ui.inlay_colors[0].foreground.over(Color::Rgb(96, 96, 96)).unwrap(), 176),
        (palette.ui.fg_breadcrumb, 192),
        (palette.ui.fg_title, 208),
        (palette.ui.fg_terminal_selection.unwrap(), 224),
        (palette.fg_selected, 240),
        (palette.ui.fg_section, 144),
        (palette.ui.fg_overview_error, 160),
        (palette.ui.fg_minimap_error, 176),
        (palette.ui.fg_line_number, 192),
    ] {
        assert_eq!(foreground, Color::Rgb(expected, expected, expected));
    }
    for (index, expected) in [144, 160, 176, 192].into_iter().enumerate() {
        let control = palette.ui.settings_controls[index];
        let background = control.background.over(palette.bg_editor).unwrap();
        assert_eq!(
            control.foreground.over(background),
            Some(Color::Rgb(expected, expected, expected))
        );
    }
}

#[test]
fn default_reset_and_invalid_overrides_reapply_without_modifying_the_source_theme() {
    let original = theme(
        serde_json::json!({
            "editor.background":"#101020", "editor.foreground":"#f0f0f0",
            "button.background":"#ff00ff", "settings.textInputBorder":"#112233"
        }),
        "vs",
    );
    let saved = original.clone();
    let overrides = serde_json::from_value(serde_json::json!({
        "editor.background":"#eeeeee", "button.background":"default",
        "[Audit]": {"editor.foreground":"invalid", "settings.textInputBorder":"#0000"}
    }))
    .unwrap();
    let customized = crate::vscode_theme::customized_theme(&original, &overrides).to_theme_colors();
    assert!(original == saved);
    assert_eq!(customized.bg_editor, Color::Rgb(238, 238, 238));
    assert_eq!(customized.fg_text, Color::Rgb(240, 240, 240));
    assert_eq!(customized.bg_sc_button, Color::Rgb(0, 122, 204));
    assert_eq!(
        customized
            .ui
            .settings_control("text")
            .border
            .border(customized.bg_editor),
        None
    );
    let restored =
        crate::vscode_theme::customized_theme(&original, &Default::default()).to_theme_colors();
    assert_eq!(restored.bg_editor, original.to_theme_colors().bg_editor);
    assert_eq!(restored.bg_sc_button, Color::Rgb(255, 0, 255));
    assert_ne!(
        customized.bg_diff_added,
        original.to_theme_colors().bg_diff_added,
        "missing translucent decorations must follow the overridden editor background"
    );
}

#[test]
fn toolbar_states_use_toolbar_colors_on_the_actual_surface_and_preserve_alpha() {
    let palette = theme(
        serde_json::json!({
            "toolbar.hoverBackground":"#ffffff80", "toolbar.activeBackground":"#00000040",
            "toolbar.hoverOutline":"#ff0000", "focusBorder":"#00ff00",
            "list.hoverBackground":"#ff00ff", "list.focusBackground":"#0000ff"
        }),
        "vs-dark",
    )
    .to_theme_colors();
    for (surface, expected) in [
        (Color::Rgb(16, 16, 16), Color::Rgb(136, 136, 136)),
        (Color::Rgb(240, 240, 240), Color::Rgb(248, 248, 248)),
    ] {
        let idle =
            palette
                .ui
                .toolbar_style(Color::Rgb(255, 255, 255), surface, false, false, false);
        let hover =
            palette
                .ui
                .toolbar_style(Color::Rgb(255, 255, 255), surface, true, false, false);
        assert_eq!(idle.bg, Some(surface));
        assert_eq!(hover.bg, Some(expected));
        assert_eq!(hover.underline_color, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(hover.fg, Some(Color::Rgb(255, 255, 255)));
        assert_eq!(
            palette
                .ui
                .toolbar_style(Color::Rgb(255, 255, 255), surface, false, true, false)
                .underline_color,
            Some(Color::Rgb(0, 255, 0))
        );
    }
}

#[test]
fn action_buttons_preserve_subtle_hover_fill_and_show_high_contrast_outlines() {
    let palette = theme(serde_json::json!({
        "button.background":"#202020", "button.hoverBackground":"#212121", "button.foreground":"#ffffff",
        "button.border":"#00ff00"
    }), "vs-dark").to_theme_colors();
    let mut terminal = Terminal::new(TestBackend::new(24, 2)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_widgets::draw_action_button(
                frame,
                Rect::new(0, 0, 24, 1),
                "Action",
                &palette,
                true,
                true,
                true,
            )
        })
        .unwrap();
    let action = &terminal.backend().buffer()[(3, 0)];
    assert_eq!(action.bg, Color::Rgb(33, 33, 33));
    assert_eq!(action.fg, Color::Rgb(255, 255, 255));
    assert_eq!(action.underline_color, Color::Rgb(0, 255, 0));
    for kind in ["hc-black", "hc-light"] {
        let palette = theme(serde_json::json!({}), kind).to_theme_colors();
        let style =
            crate::ui_widgets::action_button_style(&palette, palette.bg_sidebar, true, false, true);
        assert!(style.add_modifier.contains(Modifier::UNDERLINED), "{kind}");
        assert!(contrast_ratio(style.fg.unwrap(), style.bg.unwrap()).unwrap() >= 4.5);
        assert!(palette.ui.border_diff_added.is_some());
        assert!(palette.ui.border_diff_removed.is_some());
    }
}

#[test]
fn activity_badges_and_panel_tabs_do_not_inherit_list_colors() {
    let mut app = App::test_new();
    app.colors = theme(serde_json::json!({
        "activityBar.background":"#111111", "activityBar.foreground":"#eeeeee", "activityBar.activeBorder":"#ff8800",
        "activityBarBadge.background":"#ffffff", "activityBarBadge.foreground":"#000000",
        "panel.background":"#ffffee", "panelTitle.activeForeground":"#111111", "panelTitle.inactiveForeground":"#222222",
        "panelTitle.activeBorder":"#006600", "terminal.tab.activeBorder":"#880000",
        "list.activeSelectionForeground":"#ffff00", "list.hoverBackground":"#00ff00"
    }), "vs-dark").to_theme_colors();
    app.source_control_files.push(crate::app::GitFileChange {
        path: "example.rs".into(),
        original_path: None,
        status: 'M',
        staged: false,
    });
    app.source_control_repository_state = crate::app::SourceControlRepositoryState::Available;
    app.panel_tab = 1;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    let mut terminal = Terminal::new(TestBackend::new(120, 21)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_chrome::draw_activity_bar(frame, &app, Rect::new(0, 0, 6, 21));
            crate::ui_panels::draw_bottom_panel(frame, &mut app, Rect::new(6, 0, 114, 6));
        })
        .unwrap();
    assert_eq!(
        terminal.backend().buffer()[(0, 0)].fg,
        Color::Rgb(255, 136, 0)
    );
    let badge = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .find(|cell| cell.symbol() == "1" && cell.bg == Color::Rgb(255, 255, 255))
        .unwrap();
    assert_eq!(badge.fg, Color::Rgb(0, 0, 0));
    let output = &terminal.backend().buffer()[(7, 0)];
    assert_eq!(output.fg, Color::Rgb(17, 17, 17));
    assert_eq!(output.bg, Color::Rgb(255, 255, 238));
    assert_eq!(output.underline_color, Color::Rgb(0, 102, 0));
}

#[test]
fn colored_terminal_selection_preserves_authored_fill_and_output() {
    let palette = theme(
        serde_json::json!({
            "terminal.background":"#ffffff", "terminal.selectionBackground":"#ffffff",
            "terminal.ansiRed":"#ff0000"
        }),
        "vs",
    )
    .to_theme_colors();
    assert_eq!(
        palette.ui.terminal_color(Color::Indexed(1), true),
        Color::Rgb(255, 0, 0)
    );
    assert_eq!(
        palette.ui.terminal_color(Color::Rgb(3, 4, 5), true),
        Color::Rgb(3, 4, 5)
    );
    assert_eq!(
        palette.ui.terminal_color(Color::Indexed(90), false),
        Color::Indexed(90)
    );
    assert!(
        palette
            .ui
            .ansi
            .iter()
            .all(|color| matches!(color, Color::Rgb(..)))
    );
    let mut selection = crate::ui_colors::TerminalSelectionStyle::new(&palette.ui);
    let selected = selection.apply(
        Style::default()
            .fg(Color::Rgb(255, 0, 0))
            .add_modifier(Modifier::ITALIC | Modifier::DIM),
        palette.ui.fg_terminal,
    );
    assert_eq!(selected.bg, Some(Color::Rgb(255, 255, 255)));
    assert_eq!(selected.fg, Some(Color::Rgb(255, 0, 0)));
    assert!(selected.add_modifier.contains(Modifier::ITALIC));
    assert!(selected.sub_modifier.contains(Modifier::DIM));
    let mut terminal = Terminal::new(TestBackend::new(8, 1)).unwrap();
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new("selected").style(selected), frame.area()))
        .unwrap();
    let cell = &terminal.backend().buffer()[(0, 0)];
    assert_eq!(cell.fg, Color::Rgb(255, 0, 0));
    assert_eq!(cell.bg, Color::Rgb(255, 255, 255));
}

#[test]
fn semantic_transparency_recomposites_when_editor_background_changes() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("semantic.json");
    std::fs::write(
        &file,
        serde_json::json!({
            "name":"Semantic", "colors":{"editor.background":"#101010"},
            "semanticTokenColors":{"type":"#ffffff80"}
        })
        .to_string(),
    )
    .unwrap();
    let original = crate::vscode_theme::load_theme_file(&file, None, Some("vs-dark")).unwrap();
    let overrides =
        serde_json::from_value(serde_json::json!({"editor.background":"#e0e0e0"})).unwrap();
    let changed = crate::vscode_theme::customized_theme(&original, &overrides);
    assert_eq!(
        original.semantic_colors.as_ref().unwrap()["type"],
        (136, 136, 136)
    );
    assert_eq!(
        changed.semantic_colors.as_ref().unwrap()["type"],
        (240, 240, 240)
    );
    let reset = crate::vscode_theme::customized_theme(&original, &Default::default());
    assert_eq!(reset.semantic_colors, original.semantic_colors);
}

#[test]
fn inactive_window_title_uses_its_own_foreground_and_background() {
    let mut app = App::test_new();
    app.colors = theme(
        serde_json::json!({
            "titleBar.activeBackground":"#111111", "titleBar.activeForeground":"#eeeeee",
            "titleBar.inactiveBackground":"#eeeeee", "titleBar.inactiveForeground":"#222222"
        }),
        "vs-dark",
    )
    .to_theme_colors();
    app.terminal_has_focus = false;
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    let mut terminal = Terminal::new(TestBackend::new(160, 1)).unwrap();
    terminal
        .draw(|frame| crate::ui_chrome::draw_menu_bar(frame, &app, frame.area()))
        .unwrap();
    let file = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .find(|cell| cell.symbol() == "F")
        .unwrap();
    assert_eq!(file.bg, Color::Rgb(238, 238, 238));
    assert_eq!(file.fg, Color::Rgb(34, 34, 34));
}

#[test]
fn occurrence_bracket_and_search_toggle_roles_are_independent() {
    let palette = theme(serde_json::json!({
        "editor.background":"#ffffff", "editor.wordHighlightBackground":"#ddeedd",
        "editor.selectionHighlightBackground":"#eeeecc", "editor.selectionHighlightBorder":"#006600",
        "editorBracketMatch.background":"#ccccdd", "editorBracketMatch.border":"#000088",
        "inputOption.activeBackground":"#eebb88", "inputOption.activeForeground":"#111111",
        "inputOption.activeBorder":"#006633", "inputOption.hoverBackground":"#bbbbbb",
        "list.activeSelectionBackground":"#ff00ff", "tab.activeBorderTop":"#0000",
        "sideBar.border":"#0000", "activityBar.activeBorder":"#0000",
        "activityBar.activeBackground":"#ff0000", "diffEditor.border":"#0000"
    }), "hc-light").to_theme_colors();
    let token = Style::default().fg(Color::Rgb(51, 51, 51));
    let selected = palette
        .ui
        .occurrence_style(token, palette.bg_word_highlight, true);
    assert_eq!(selected.bg, Some(Color::Rgb(238, 238, 204)));
    assert_eq!(selected.underline_color, Some(Color::Rgb(0, 102, 0)));
    assert_eq!(
        palette
            .ui
            .occurrence_style(token, palette.bg_word_highlight, false)
            .bg,
        Some(Color::Rgb(221, 238, 221))
    );
    let bracket = palette.ui.bracket_style(token, palette.bg_bracket_match);
    assert_eq!(bracket.underline_color, Some(Color::Rgb(0, 0, 136)));
    assert_eq!(bracket.fg, token.fg);
    let toggle =
        palette
            .ui
            .input_option_style(Color::Rgb(51, 51, 51), palette.bg_sidebar, true, false);
    assert_eq!(toggle.bg, Some(Color::Rgb(238, 187, 136)));
    assert_eq!(toggle.fg, Some(Color::Rgb(17, 17, 17)));
    assert_eq!(toggle.underline_color, Some(Color::Rgb(0, 102, 51)));
    assert_eq!(palette.ui.border_sidebar, None);
    assert_eq!(palette.ui.border_diff, None);
    assert_eq!(
        palette
            .ui
            .border_activity_active
            .border(Color::Rgb(255, 0, 0)),
        None
    );
    assert_eq!(
        palette.ui.border_tab_active, None,
        "transparent top borders must suppress HC fallbacks"
    );
}

#[test]
fn default_button_hover_colors_preserve_hue_and_high_contrast_fill() {
    let dark = theme(serde_json::json!({}), "vs-dark").to_theme_colors();
    let light = theme(serde_json::json!({}), "vs").to_theme_colors();
    assert_eq!(
        dark.ui.primary_button_hover.over(dark.bg_sc_button),
        Some(Color::Rgb(17, 119, 187))
    );
    assert_eq!(
        light.ui.primary_button_hover.over(light.bg_sc_button),
        Some(Color::Rgb(0, 98, 163))
    );
    assert!(
        dark.ui
            .secondary_button
            .border
            .border(dark.bg_sc_button_secondary)
            .is_some()
    );
    assert!(
        light
            .ui
            .secondary_button
            .border
            .border(light.bg_sc_button_secondary)
            .is_some()
    );
    for kind in ["hc-black", "hc-light"] {
        let palette = theme(serde_json::json!({}), kind).to_theme_colors();
        assert_eq!(
            palette.ui.primary_button_hover.or(palette.ui.primary_button.background).over(palette.bg_sc_button),
            Some(palette.bg_sc_button)
        );
        assert_eq!(
            palette.ui.button_style(palette.bg_sc_button_secondary, false, true, true).bg,
            Some(palette.bg_sc_button_secondary)
        );
    }
}

#[test]
fn picker_title_defaults_follow_vscode_theme_kind_and_explicit_transparency() {
    let surface = Color::Rgb(40, 80, 120);
    // Registered quickInputTitle defaults: white 10.5%, black 6%,
    // opaque HC black, and opaque HC white, respectively.
    for (kind, expected) in [
        ("vs-dark", Color::Rgb(63, 99, 134)),
        ("vs", Color::Rgb(38, 75, 113)),
        ("hc-black", Color::Rgb(0, 0, 0)),
        ("hc-light", Color::Rgb(255, 255, 255)),
    ] {
        let mut source = theme(
            serde_json::json!({
                "editorWidget.foreground":"#aaffbb",
                "quickInput.foreground":"#ffaacc"
            }),
            kind,
        );
        // Neither the display name nor either text color determines this fill.
        source.name = if source.is_light { "Dark" } else { "Light" }.into();
        assert_eq!(
            source
                .to_theme_colors()
                .ui
                .quick_title_background
                .over(surface),
            Some(expected),
            "{kind}"
        );
        source
            .workbench_colors
            .insert("quickInputTitle.background".into(), "#ff000000".into());
        assert_eq!(
            source
                .to_theme_colors()
                .ui
                .quick_title_background
                .over(surface),
            Some(surface),
            "{kind}: authored transparency"
        );
    }
}

#[test]
fn completion_defaults_inherit_editor_quick_input_focus_and_high_contrast_roles() {
    let surface = Color::Rgb(16, 32, 48);
    for kind in ["vs-dark", "vs", "hc-black", "hc-light"] {
        let mut source = theme(
            serde_json::json!({
                "editor.foreground":"#ffeecc", "editorWidget.foreground":"#bbffee",
                "editorSuggestWidget.background":"#102030", "focusBorder":"#dd66dd",
                "quickInput.foreground":"#cceeff", "quickInputList.focusBackground":"#304050",
                "quickInputList.focusForeground":"#bbffdd", "quickInputList.focusIconForeground":"#ffddbb",
                "list.highlightForeground":"#ffdd99", "list.focusHighlightForeground":"#bbff99"
            }),
            kind,
        );
        let palette = source.to_theme_colors();
        let selected_bg = if source.high_contrast {
            Color::Rgb(255, 238, 204)
        } else {
            Color::Rgb(48, 64, 80)
        };
        let selected_fg = if source.high_contrast {
            surface
        } else {
            Color::Rgb(187, 255, 221)
        };
        let selected = palette.ui.suggest_row_style(surface, true, false);
        assert_eq!(selected.bg, Some(selected_bg), "{kind}");
        assert_eq!(selected.fg, Some(selected_fg), "{kind}");
        assert_eq!(
            palette.ui.suggest_row_style(surface, false, false).fg,
            Some(Color::Rgb(255, 238, 204)),
            "{kind}: editor text inheritance"
        );
        assert_eq!(
            palette.ui.completion_icon_color(
                Some(lsp_types::CompletionItemKind::METHOD),
                selected_bg,
                true
            ),
            if source.high_contrast {
                surface
            } else {
                Color::Rgb(255, 221, 187)
            },
            "{kind}: selected icon inheritance"
        );
        assert_eq!(
            palette.ui.suggest_match_color(selected_bg, true),
            if source.high_contrast {
                surface
            } else {
                Color::Rgb(187, 255, 153)
            },
            "{kind}: focused match inheritance"
        );
        assert_eq!(
            selected.underline_color,
            source.high_contrast.then_some(Color::Rgb(221, 102, 221)),
            "{kind}: inherited focus outline"
        );

        // A fully transparent authored outline disables the HC default, and
        // a fully transparent selected fill is not a missing color.
        source.workbench_colors.insert(
            "editorSuggestWidget.focusOutline".into(),
            "#ff000000".into(),
        );
        source.workbench_colors.insert(
            "editorSuggestWidget.selectedBackground".into(),
            "#ff000000".into(),
        );
        let palette = source.to_theme_colors();
        assert_eq!(
            palette.ui.suggest_selection.background.over(surface),
            Some(surface)
        );
        assert_eq!(palette.ui.suggest_selection.border.border(surface), None);
    }
    for kind in ["hc-black", "hc-light"] {
        let palette = theme(
            serde_json::json!({
                "quickInput.foreground":"#cceeff",
                "quickInput.list.focusBackground":"#ff0000"
            }),
            kind,
        )
        .to_theme_colors();
        assert_eq!(
            palette.ui.quick_focus_background.over(surface),
            None,
            "{kind}: deprecated alias must not change the registered HC default"
        );
        assert_eq!(
            palette.ui.quick_text_color(surface, true),
            Color::Rgb(204, 238, 255),
            "{kind}: null focus foreground inherits quick input text"
        );
    }
}

#[test]
fn bundled_controls_render_their_registered_foregrounds_and_fills() {
    let themes = crate::app_theme::load_builtin_themes();
    assert!(themes.iter().any(|theme| theme.is_light));
    assert!(themes.iter().any(|theme| theme.high_contrast));
    assert!(themes.iter().any(|theme| theme.config_key == "monokai"));
    for theme in themes {
        let palette =
            crate::vscode_theme::customized_theme(&theme, &Default::default()).to_theme_colors();
        for primary in [false, true] {
            for hovered in [false, true] {
                let mut terminal = Terminal::new(TestBackend::new(18, 2)).unwrap();
                terminal
                    .draw(|frame| {
                        crate::ui_widgets::draw_action_button(
                            frame,
                            Rect::new(0, 0, 18, 1),
                            "Action",
                            &palette,
                            primary,
                            hovered,
                            true,
                        );
                        let style = palette.ui.toolbar_style(
                            palette.ui.fg_sidebar,
                            palette.bg_sidebar,
                            hovered,
                            false,
                            false,
                        );
                        frame.render_widget(
                            Paragraph::new("Toolbar").style(style),
                            Rect::new(0, 1, 18, 1),
                        );
                    })
                    .unwrap();
                let control = if primary { palette.ui.primary_button } else { palette.ui.secondary_button };
                let fill = if !hovered { control.background } else if primary {
                    palette.ui.primary_button_hover.or(control.background)
                } else { palette.ui.secondary_button_hover.or(control.background) };
                let bg = fill.over(palette.ui.bg_widget).unwrap_or(palette.ui.bg_widget);
                let cell = &terminal.backend().buffer()[(5, 0)];
                assert_eq!(cell.bg, bg, "{}: button fill", theme.name);
                assert_eq!(cell.fg, control.foreground.over(bg).unwrap(), "{}: button text", theme.name);
                assert_eq!(terminal.backend().buffer()[(1, 1)].fg, palette.ui.fg_sidebar, "{}: toolbar text", theme.name);
            }
        }
    }
}

#[test]
fn chat_markdown_preserves_authored_text_even_when_the_theme_is_low_contrast() {
    let palette = theme(
        serde_json::json!({
            "sideBar.background":"#eeeeee", "sideBar.foreground":"#eeeeee",
            "textPreformat.background":"#111111", "textPreformat.foreground":"#222222"
        }),
        "vs",
    )
    .to_theme_colors();
    let lines = crate::ui_markdown::chat_lines("Text with `code`", 40, &palette, None);
    let mut terminal = Terminal::new(TestBackend::new(40, 4)).unwrap();
    terminal
        .draw(|frame| {
            frame.render_widget(
                Paragraph::new(lines.clone()).style(Style::default().bg(palette.bg_sidebar)),
                frame.area(),
            )
        })
        .unwrap();
    for cell in &terminal.backend().buffer().content {
        if !cell.symbol().trim().is_empty() {
            let expected = if cell.bg == Color::Rgb(17, 17, 17) {
                Color::Rgb(34, 34, 34)
            } else { Color::Rgb(238, 238, 238) };
            assert_eq!(cell.fg, expected);
        }
    }
}
