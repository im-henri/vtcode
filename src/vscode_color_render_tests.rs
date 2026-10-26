//! Check role ownership all the way from upstream fixtures to runtime palettes.
use ratatui::style::Color;
use serde_json::{Map, Value};

use crate::theme_workbench::ColorLayer;

fn expected_layer(expected: &Map<String, Value>, id: &str) -> ColorLayer {
    let value = expected
        .get(id)
        .unwrap_or_else(|| panic!("role missing from upstream fixture: {id}"));
    let Some(rgba) = value.as_array() else {
        return ColorLayer::default();
    };
    ColorLayer::rgba(
        rgba[0].as_u64().unwrap() as u8,
        rgba[1].as_u64().unwrap() as u8,
        rgba[2].as_u64().unwrap() as u8,
        (rgba[3].as_f64().unwrap() * 255.0).round() as u8,
    )
}

#[test]
fn imported_workbench_layers_match_upstream_roles_on_different_surfaces() {
    let fixtures: Value = serde_json::from_str(include_str!(
        "../tests/fixtures/vscode-color-reference.json"
    ))
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("theme.json");
    for case in fixtures["cases"].as_array().unwrap() {
        let kind = case["kind"].as_str().unwrap();
        let ui_theme = match kind {
            "dark" => "vs-dark",
            "light" => "vs",
            "hcDark" => "hc-black",
            _ => "hc-light",
        };
        std::fs::write(
            &path,
            serde_json::json!({"colors":case["authored"]}).to_string(),
        )
        .unwrap();
        let theme = crate::vscode_theme::load_theme_file(&path, None, Some(ui_theme)).unwrap();
        let colors = theme.to_theme_colors();
        let ui = &colors.ui;
        assert!(ui.preserve_theme_colors);
        let expected = case["expected"].as_object().unwrap();
        let check = |actual: ColorLayer, id: &str| {
            let expected = expected_layer(expected, id);
            for surface in [
                Color::Rgb(0, 0, 0),
                Color::Rgb(255, 255, 255),
                Color::Rgb(21, 38, 57),
            ] {
                assert_eq!(
                    actual.over(surface),
                    expected.over(surface),
                    "{kind}: {id} on {surface:?}"
                );
            }
        };
        macro_rules! layers {
            ($($field:ident => $role:literal),* $(,)?) => { $(check(ui.$field, $role);)* };
        }
        layers! {
            workbench_foreground => "foreground", description_foreground => "descriptionForeground",
            editor_foreground => "editor.foreground", ruler_foreground => "editorRuler.foreground",
            codelens_foreground => "editorCodeLens.foreground", sticky_background => "editorStickyScroll.background",
            sticky_hover_background => "editorStickyScrollHover.background", sticky_border => "editorStickyScroll.border",
            sidebar_foreground => "sideBar.foreground", sidebar_title_foreground => "sideBarTitle.foreground",
            section_foreground => "sideBarSectionHeader.foreground", sidebar_sticky_background => "sideBarStickyScroll.background",
            graph_lane_foreground => "scmGraph.foreground1", graph_reference_foreground => "scmGraph.historyItemHoverLabelForeground",
            widget_background => "editorWidget.background", widget_foreground => "editorWidget.foreground",
            widget_border => "editorWidget.border", merge_border => "merge.border", sidebar_border => "sideBar.border",
            sash_hover_border => "sash.hoverBorder", line_number_foreground => "editorLineNumber.foreground",
            error_foreground => "errorForeground", warning_foreground => "editorWarning.foreground",
            disabled_foreground => "disabledForeground", primary_button_hover => "button.hoverBackground",
            secondary_button_hover => "button.secondaryHoverBackground", keybinding_label_background => "keybindingLabel.background",
            keybinding_label_foreground => "keybindingLabel.foreground", keybinding_label_border => "keybindingLabel.bottomBorder",
            keybinding_table_header => "keybindingTable.headerBackground", keybinding_table_row => "keybindingTable.rowsBackground",
            badge_background => "badge.background", badge_foreground => "badge.foreground", icon_foreground => "icon.foreground",
            folder_icon_foreground => "icon.foreground", list_active_icon => "list.activeSelectionIconForeground",
            list_inactive_icon => "list.inactiveSelectionIconForeground", list_match_foreground => "list.highlightForeground",
            list_focus_match_foreground => "list.focusHighlightForeground", scrollbar_background => "scrollbar.background",
            scrollbar_slider => "scrollbarSlider.background", scrollbar_slider_hover => "scrollbarSlider.hoverBackground",
            scrollbar_slider_active => "scrollbarSlider.activeBackground", suggest_highlight => "editorSuggestWidget.highlightForeground",
            suggest_focus_highlight => "editorSuggestWidget.focusHighlightForeground", suggest_selected_icon => "editorSuggestWidget.selectedIconForeground",
            input_placeholder => "input.placeholderForeground", settings_row_border => "settings.focusedRowBorder",
            settings_header_foreground => "settings.headerForeground", list_active_background => "list.activeSelectionBackground",
            list_active_foreground => "list.activeSelectionForeground", list_inactive_background => "list.inactiveSelectionBackground",
            list_inactive_foreground => "list.inactiveSelectionForeground", list_hover_background => "list.hoverBackground",
            list_hover_foreground => "list.hoverForeground", list_drop_background => "list.dropBackground",
            list_focus_background => "list.focusBackground", list_focus_foreground => "list.focusForeground",
            list_focused_outline => "list.focusOutline", list_error_foreground => "list.errorForeground",
            list_inactive_focus_outline => "list.inactiveFocusOutline", action_list_background => "editorActionList.background",
            action_list_foreground => "editorActionList.foreground", action_list_focus_background => "editorActionList.focusBackground",
            action_list_focus_foreground => "editorActionList.focusForeground", quick_foreground => "quickInput.foreground",
            quick_background => "quickInput.background", quick_focus_background => "quickInputList.focusBackground",
            quick_focus_foreground => "quickInputList.focusForeground", quick_focus_highlight => "quickInputList.focusHighlightForeground",
            quick_focus_icon => "quickInputList.focusIconForeground", quick_title_background => "quickInputTitle.background",
            peek_border => "peekView.border", peek_title_background => "peekViewTitle.background",
            peek_title_foreground => "peekViewTitleLabel.foreground", peek_description_foreground => "peekViewTitleDescription.foreground",
            peek_editor_background => "peekViewEditor.background", peek_gutter_background => "peekViewEditorGutter.background",
            command_center_background => "commandCenter.background", command_center_active_background => "commandCenter.activeBackground",
            command_center_foreground => "commandCenter.foreground", command_center_active_foreground => "commandCenter.activeForeground",
            command_center_inactive_foreground => "commandCenter.inactiveForeground", command_center_border => "commandCenter.border",
            command_center_active_border => "commandCenter.activeBorder", command_center_inactive_border => "commandCenter.inactiveBorder",
            menubar_background => "menubar.selectionBackground", menubar_foreground => "menubar.selectionForeground",
            menubar_border => "menubar.selectionBorder", md_inline_background => "textPreformat.background",
            md_inline_foreground => "textPreformat.foreground", md_code_background => "textCodeBlock.background",
            md_quote_background => "textBlockQuote.background", md_quote_border => "textBlockQuote.border",
            chat_request_hover_background => "chat.requestBubbleHoverBackground", focus_border => "focusBorder",
            contrast_border => "contrastBorder", active_contrast_border => "contrastActiveBorder",
            toolbar_hover => "toolbar.hoverBackground", toolbar_active => "toolbar.activeBackground",
            toolbar_outline => "toolbar.hoverOutline", input_option_background => "inputOption.activeBackground",
            input_option_foreground => "inputOption.activeForeground", input_option_border => "inputOption.activeBorder",
            input_option_hover => "inputOption.hoverBackground", bg_activity_active => "activityBar.activeBackground",
            border_activity_active => "activityBar.activeBorder", menu_selection_border => "menu.selectionBorder",
            search_match_border => "editor.findMatchHighlightBorder",
        }
        // Search view CSS suppresses match fills for high contrast themes.
        if kind.starts_with("hc") {
            assert_eq!(ui.search_match_background.over(Color::Black), None);
        } else {
            check(ui.search_match_background, "editor.findMatchHighlightBackground");
        }
        for (control, prefix) in [
            (ui.input_colors, "input"),
            (ui.primary_button, "button"),
            (ui.hover_colors, "editorHoverWidget"),
            (ui.suggest_colors, "editorSuggestWidget"),
            (ui.dropdown_colors, "dropdown"),
            (ui.menu_colors, "menu"),
        ] {
            check(control.background, &format!("{prefix}.background"));
            check(control.foreground, &format!("{prefix}.foreground"));
            check(control.border, &format!("{prefix}.border"));
        }
        check(ui.secondary_button.background, "button.secondaryBackground");
        check(ui.secondary_button.foreground, "button.secondaryForeground");
        check(ui.secondary_button.border, "button.secondaryBorder");
        check(ui.chat_avatar.background, "chat.avatarBackground");
        check(ui.chat_avatar.foreground, "chat.avatarForeground");
        for (index, name) in [
            "added",
            "deleted",
            "modified",
            "untracked",
            "renamed",
            "ignored",
        ]
        .into_iter()
        .enumerate()
        {
            check(
                ui.git_decorations[index],
                &format!("gitDecoration.{name}ResourceForeground"),
            );
        }
        for (index, name) in ["current", "incoming", "common"].into_iter().enumerate() {
            check(
                ui.merge_headers[index],
                &format!("merge.{name}HeaderBackground"),
            );
        }
        for (index, name) in ["Info", "Warning", "Error"].into_iter().enumerate() {
            check(
                ui.notification_icons[index],
                &format!("notifications{name}Icon.foreground"),
            );
        }
        for (index, name) in [
            "array",
            "boolean",
            "class",
            "color",
            "constant",
            "constructor",
            "enumerator",
            "enumeratorMember",
            "event",
            "field",
            "file",
            "folder",
            "function",
            "interface",
            "key",
            "keyword",
            "method",
            "module",
            "namespace",
            "null",
            "number",
            "object",
            "operator",
            "package",
            "property",
            "reference",
            "snippet",
            "string",
            "struct",
            "text",
            "typeParameter",
            "unit",
            "variable",
        ]
        .into_iter()
        .enumerate()
        {
            check(
                ui.symbol_icons[index],
                &format!("symbolIcon.{name}Foreground"),
            );
        }
        for (index, name) in ["checkbox", "dropdown", "numberInput", "textInput"]
            .into_iter()
            .enumerate()
        {
            for (layer, suffix) in [
                (ui.settings_controls[index].background, "Background"),
                (ui.settings_controls[index].foreground, "Foreground"),
                (ui.settings_controls[index].border, "Border"),
            ] {
                check(layer, &format!("settings.{name}{suffix}"));
            }
        }
        for (actual, id, owner) in [
            (
                ui.fg_whitespace,
                "editorWhitespace.foreground",
                colors.bg_editor,
            ),
            (
                ui.fg_tab_active,
                "tab.activeForeground",
                colors.bg_tab_active,
            ),
            (ui.fg_minimap_error, "minimap.errorHighlight", ui.bg_minimap),
            (
                ui.fg_minimap_warning,
                "minimap.warningHighlight",
                ui.bg_minimap,
            ),
            (ui.fg_minimap_info, "minimap.infoHighlight", ui.bg_minimap),
            (
                ui.fg_minimap_added,
                "minimapGutter.addedBackground",
                ui.bg_minimap,
            ),
            (
                ui.fg_minimap_modified,
                "minimapGutter.modifiedBackground",
                ui.bg_minimap,
            ),
            (
                ui.fg_minimap_deleted,
                "minimapGutter.deletedBackground",
                ui.bg_minimap,
            ),
            (
                ui.bg_minimap_slider,
                "minimapSlider.background",
                ui.bg_minimap,
            ),
            (
                ui.bg_minimap_slider_hover,
                "minimapSlider.hoverBackground",
                ui.bg_minimap,
            ),
            (
                ui.bg_minimap_slider_active,
                "minimapSlider.activeBackground",
                ui.bg_minimap,
            ),
            (
                ui.fg_overview_error,
                "editorOverviewRuler.errorForeground",
                ui.bg_overview,
            ),
            (
                ui.fg_overview_warning,
                "editorOverviewRuler.warningForeground",
                ui.bg_overview,
            ),
            (
                ui.fg_overview_info,
                "editorOverviewRuler.infoForeground",
                ui.bg_overview,
            ),
            (
                ui.fg_overview_added,
                "editorOverviewRuler.addedForeground",
                ui.bg_overview,
            ),
            (
                ui.fg_overview_modified,
                "editorOverviewRuler.modifiedForeground",
                ui.bg_overview,
            ),
            (
                ui.fg_overview_deleted,
                "editorOverviewRuler.deletedForeground",
                ui.bg_overview,
            ),
        ] {
            assert_eq!(
                actual,
                expected_layer(expected, id).over(owner).unwrap_or(owner),
                "{kind}: scalar {id}"
            );
        }
        // Runtime defaults must not pollute the editable authored map.
        assert_eq!(
            &theme.workbench_colors,
            case["authored"].as_object().unwrap()
        );
    }
}

#[test]
fn imported_low_contrast_colors_survive_editor_and_terminal_selection() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("muted.json");
    std::fs::write(&path, serde_json::json!({"colors":{
        "foreground":"#242424", "descriptionForeground":"#292929", "editor.background":"#202020",
        "editor.selectionBackground":"#212121", "terminal.background":"#202020", "terminal.selectionBackground":"#212121"
    }}).to_string()).unwrap();
    let colors = crate::vscode_theme::load_theme_file(&path, None, Some("vs-dark"))
        .unwrap()
        .to_theme_colors();
    assert_eq!(
        colors.ui.workbench_text_color(colors.bg_editor),
        Color::Rgb(36, 36, 36)
    );
    assert_eq!(
        colors.ui.description_color(colors.bg_editor),
        Color::Rgb(41, 41, 41)
    );
    let token = ratatui::style::Style::default().fg(Color::Rgb(48, 49, 50));
    let mut editor = crate::ui_colors::EditorSelectionStyle::new(&colors);
    let mut terminal = crate::ui_colors::TerminalSelectionStyle::new(&colors.ui);
    for style in [
        editor.apply(token),
        terminal.apply(token, colors.ui.fg_terminal),
    ] {
        assert_eq!(style.bg, Some(Color::Rgb(33, 33, 33)));
        assert_eq!(style.fg, token.fg);
    }
}

fn imported_colors(authored: Value, kind: &str) -> crate::app_theme::ThemeColors {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("theme.json");
    std::fs::write(&path, serde_json::json!({"colors": authored}).to_string()).unwrap();
    crate::vscode_theme::load_theme_file(&path, None, Some(kind))
        .unwrap()
        .to_theme_colors()
}

#[test]
fn vscode_color_inherited_popup_backgrounds_are_composited_once() {
    let colors = imported_colors(
        serde_json::json!({
            "editor.background": "#102030", "editorWidget.background": "#c0804040",
            "dropdown.background": "#20406080",
        }),
        "vs-dark",
    );
    let widget = ColorLayer::rgba(192, 128, 64, 64)
        .over(colors.bg_editor)
        .unwrap();
    assert_eq!(colors.ui.bg_widget, widget);
    assert_eq!(colors.ui.bg_quick_input, widget);
    assert_eq!(colors.ui.bg_notification, widget);
    assert_eq!(
        colors.bg_menu,
        ColorLayer::rgba(32, 64, 96, 128)
            .over(colors.bg_editor)
            .unwrap()
    );

    // The same inherited menu layer must be composed on its real owner, not
    // the editor or an already-composited dropdown color.
    let items = [
        crate::ui_widgets::DropdownItem::new("Normal"),
        crate::ui_widgets::DropdownItem::new("Selected"),
        crate::ui_widgets::DropdownItem::new("─"),
        crate::ui_widgets::DropdownItem::new("Disabled").disabled(true),
    ];
    let mut theme = serde_json::json!({
        "dropdown.background": "#20406080", "dropdown.foreground": "#ffffff80",
        "menu.border": "#ffaa0080", "menu.separatorBackground": "#ff000080",
        "menu.selectionBackground": "#30609080", "menu.selectionForeground": "#ffffffc0",
        "disabledForeground": "#bbaa9980",
    });
    // Distinct sidebar/editor surfaces expose accidental eager composition.
    theme["editor.background"] = "#102030".into();
    theme["sideBar.background"] = "#506070".into();
    let colors = imported_colors(theme, "vs-dark");
    for owner in [
        Color::Rgb(16, 32, 48),
        Color::Rgb(80, 96, 112),
        Color::Rgb(240, 248, 255),
    ] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(32, 8)).unwrap();
        let mut inner = ratatui::layout::Rect::default();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("")
                        .style(ratatui::style::Style::default().bg(owner)),
                    frame.area(),
                );
                inner = crate::ui_widgets::DropdownMenu::new(
                    &items,
                    crate::ui_widgets::themed_dropdown_colors(&colors),
                )
                .anchor(2, 1)
                .selected(1)
                .render(frame);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let bg = ColorLayer::rgba(32, 64, 96, 128).over(owner).unwrap();
        let selected_bg = ColorLayer::rgba(48, 96, 144, 128).over(bg).unwrap();
        let normal = &buffer[(inner.x + 1, inner.y)];
        assert_eq!(normal.bg, bg);
        assert_eq!(
            normal.fg,
            ColorLayer::rgba(255, 255, 255, 128).over(bg).unwrap()
        );
        let selected = &buffer[(inner.x + 1, inner.y + 1)];
        assert_eq!(selected.bg, selected_bg);
        assert_eq!(
            selected.fg,
            ColorLayer::rgba(255, 255, 255, 192)
                .over(selected_bg)
                .unwrap()
        );
        assert_eq!(
            buffer[(inner.x, inner.y + 2)].fg,
            ColorLayer::rgba(255, 0, 0, 128).over(bg).unwrap()
        );
        assert_eq!(
            buffer[(inner.x + 1, inner.y + 3)].fg,
            ColorLayer::rgba(187, 170, 153, 128).over(bg).unwrap()
        );
        assert_eq!(
            buffer[(inner.x - 1, inner.y)].fg,
            ColorLayer::rgba(255, 170, 0, 128).over(bg).unwrap()
        );
    }
}

#[test]
fn vscode_color_settings_options_use_select_box_focus_roles() {
    let colors = imported_colors(
        serde_json::json!({
            "settings.dropdownBackground": "#20406080", "settings.dropdownForeground": "#cccccc",
            "dropdown.listBackground": "#20406080",
            "list.activeSelectionBackground": "#ff0000", "list.activeSelectionForeground": "#ffff00",
            "list.focusOutline": "#00ff00", "quickInputList.focusBackground": "#30609080",
            "quickInputList.focusForeground": "#ffffffc0", "contrastActiveBorder": "#ff8800",
        }),
        "hc-black",
    );
    let owner = Color::Rgb(16, 32, 48);
    let items = [crate::ui_widgets::DropdownItem::new("Selected")];
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 6)).unwrap();
    let mut inner = ratatui::layout::Rect::default();
    terminal
        .draw(|frame| {
            frame.render_widget(
                ratatui::widgets::Paragraph::new("")
                    .style(ratatui::style::Style::default().bg(owner)),
                frame.area(),
            );
            inner = crate::ui_widgets::DropdownMenu::new(
                &items,
                crate::ui_widgets::settings_dropdown_colors(&colors),
            )
            .render(frame);
        })
        .unwrap();
    let selected = &terminal.backend().buffer()[(inner.x + 1, inner.y)];
    let bg = ColorLayer::rgba(48, 96, 144, 128)
        .over(ColorLayer::rgba(32, 64, 96, 128).over(owner).unwrap())
        .unwrap();
    assert_eq!(selected.bg, bg);
    assert_eq!(
        selected.fg,
        ColorLayer::rgba(255, 255, 255, 192).over(bg).unwrap()
    );
    assert!(
        selected
            .modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    assert_eq!(selected.underline_color, Color::Rgb(255, 136, 0));
}

#[test]
fn vscode_color_unset_dropdown_list_and_menu_border_inherit_component_styles() {
    for list_background in [None, Some("#00000000")] {
        let mut authored = serde_json::json!({
            "settings.dropdownBackground": "#30507080", "menu.background": "#30507080",
            "editorWidget.border": "#ff880080",
        });
        if let Some(value) = list_background {
            authored["dropdown.listBackground"] = value.into();
        }
        let colors = imported_colors(authored, "vs-dark");
        let owner = Color::Rgb(16, 32, 48);
        let items = [
            crate::ui_widgets::DropdownItem::new("Normal"),
            crate::ui_widgets::DropdownItem::new("Selected"),
        ];
        for settings in [false, true] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 6)).unwrap();
            let mut inner = ratatui::layout::Rect::default();
            terminal
                .draw(|frame| {
                    frame.render_widget(
                        ratatui::widgets::Paragraph::new("")
                            .style(ratatui::style::Style::default().bg(owner)),
                        frame.area(),
                    );
                    let popup_colors = if settings {
                        crate::ui_widgets::settings_dropdown_colors(&colors)
                    } else {
                        crate::ui_widgets::themed_dropdown_colors(&colors)
                    };
                    inner = crate::ui_widgets::DropdownMenu::new(&items, popup_colors)
                        .selected(1)
                        .render(frame);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let bg = if settings && list_background.is_some() {
                owner
            } else {
                ColorLayer::rgba(48, 80, 112, 128).over(owner).unwrap()
            };
            assert_eq!(
                buffer[(inner.x + 1, inner.y)].bg,
                bg,
                "unset list fill inherits; an explicit transparent fill does not"
            );
            assert_eq!(
                buffer[(inner.x - 1, inner.y)].fg,
                ColorLayer::rgba(255, 136, 0, 128).over(bg).unwrap(),
                "unset menu border inherits the widget outline on the menu surface"
            );
        }
    }
}

#[test]
fn vscode_color_disabled_buttons_retain_their_roles_with_group_opacity() {
    let colors = imported_colors(
        serde_json::json!({
            "button.background": "#204060", "button.foreground": "#d0e0f0", "button.border": "#ff8800",
            "button.secondaryBackground": "#304050", "button.secondaryForeground": "#aabbcc", "button.secondaryBorder": "#00ff80",
            "disabledForeground": "#ff0000",
        }),
        "vs-dark",
    );
    let owner = Color::Rgb(16, 32, 48);
    for (primary, fill, text, border) in [
        (true, [32, 64, 96], [208, 224, 240], [255, 136, 0]),
        (false, [48, 64, 80], [170, 187, 204], [0, 255, 128]),
    ] {
        let dim = |[r, g, b]: [u8; 3]| ColorLayer::rgba(r, g, b, 102).over(owner).unwrap();
        for hovered in [true, false] {
            let style = colors.ui.button_style(owner, primary, hovered, false);
            assert_eq!(style.bg, Some(dim(fill)));
            assert_eq!(style.fg, Some(dim(text)));
            assert_eq!(style.underline_color, Some(dim(border)));
        }
    }
}

#[test]
fn vscode_color_markdown_headings_quotes_and_transparent_code_use_their_owner() {
    let colors = imported_colors(
        serde_json::json!({
            "editor.background": "#102030", "sideBar.background": "#304050", "sideBar.foreground": "#ccddee80",
            "textBlockQuote.background": "#30507080", "textBlockQuote.border": "#ff880080",
            "textPreformat.foreground": "#eeffaa80",
        }),
        "hc-black",
    );
    for owner in [Color::Rgb(48, 64, 80), Color::Rgb(80, 64, 48)] {
        let lines = crate::ui_markdown::chat_lines_on_surface(
            "# Header\n\n> Quoted\n\n`inline`",
            40,
            &colors,
            None,
            owner,
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 14)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(lines)
                        .style(ratatui::style::Style::default().bg(owner)),
                    frame.area(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let heading = buffer
            .content
            .iter()
            .find(|cell| cell.symbol() == "H")
            .unwrap();
        assert_eq!(
            heading.fg,
            ColorLayer::rgba(204, 221, 238, 128).over(owner).unwrap()
        );
        let quote_bg = ColorLayer::rgba(48, 80, 112, 128).over(owner).unwrap();
        let quote = buffer
            .content
            .iter()
            .find(|cell| cell.symbol() == "Q")
            .unwrap();
        assert_eq!(quote.bg, quote_bg);
        assert_eq!(
            quote.fg,
            ColorLayer::rgba(204, 221, 238, 128).over(quote_bg).unwrap()
        );
        assert!(!quote.modifier.contains(ratatui::style::Modifier::ITALIC));
        let border = buffer
            .content
            .iter()
            .find(|cell| cell.symbol() == "▎")
            .unwrap();
        assert_eq!(
            border.fg,
            ColorLayer::rgba(255, 136, 0, 128).over(quote_bg).unwrap()
        );
        let code = buffer
            .content
            .iter()
            .find(|cell| cell.symbol() == "i")
            .unwrap();
        assert_eq!(
            code.bg, owner,
            "null high contrast code background inherits the message surface"
        );
        assert_eq!(
            code.fg,
            ColorLayer::rgba(238, 255, 170, 128).over(owner).unwrap()
        );
    }
}

fn rendered_nested_brackets(
    colors: crate::app_theme::ThemeColors,
    word_wrap: bool,
    selected: bool,
    colorize: bool,
) -> Vec<(char, Color, Color)> {
    let nested = format!("{}x{}", "([{".repeat(4), "}])".repeat(4));
    let source = format!("value\n{nested}\n");
    let mut app = crate::app::App::test_new();
    let mut editor = crate::editor::Editor::test_with_text(&source);
    editor.word_wrap = word_wrap;
    editor.word_wrap_column = 10;
    if selected {
        editor.selection_anchor = Some((1, 0));
        editor.cursor_line = 1;
        editor.cursor_col = nested.len();
    }
    app.tabs.push(crate::app::Tab::new_file(editor));
    app.active_tab = 0;
    app.colors = colors;
    app.show_line_numbers = false;
    app.show_minimap = false;
    app.show_indent_guides = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    app.bracket_pair_colorization = colorize;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 8)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_editor::draw_editor(
                frame,
                &mut app,
                ratatui::layout::Rect::new(0, 0, 40, 8),
            );
        })
        .unwrap();
    let code = app.code_area.unwrap();
    let buffer = terminal.backend().buffer();
    let mut brackets = Vec::new();
    for y in code.top()..code.bottom() {
        for x in code.left()..code.right() {
            let cell = &buffer[(x, y)];
            if let Some(ch @ ('(' | ')' | '[' | ']' | '{' | '}')) =
                cell.symbol().chars().next()
            {
                brackets.push((ch, cell.fg, cell.bg));
            }
        }
    }
    assert_eq!(brackets.len(), 24, "all twelve bracket pairs must be drawn");
    brackets
}

#[test]
fn vscode_color_nested_brackets_remain_visible_in_every_builtin_theme() {
    let mut themes: Vec<_> = crate::app_theme::load_builtin_themes()
        .into_iter()
        .map(|theme| (theme.name.clone(), theme.to_theme_colors()))
        .collect();
    for kind in ["vs-dark", "vs"] {
        themes.push((kind.into(), imported_colors(serde_json::json!({}), kind)));
    }
    for (name, colors) in themes {
        for word_wrap in [false, true] {
            for selected in [false, true] {
                for (index, (_, foreground, background)) in
                    rendered_nested_brackets(colors, word_wrap, selected, true)
                        .into_iter()
                        .enumerate()
                {
                    if !selected {
                        assert_ne!(
                            foreground, colors.bg_editor,
                            "{name}: bracket {index} must not use the editor background; word_wrap={word_wrap}"
                        );
                    }
                    assert_ne!(
                        foreground, background,
                        "{name}: bracket {index} must remain visible; word_wrap={word_wrap}, selected={selected}"
                    );
                }
            }
        }
    }
}

#[test]
fn vscode_color_nested_brackets_cycle_only_enabled_palette_slots() {
    let colors = imported_colors(
        serde_json::json!({
            "editorBracketHighlight.foreground1": "#ff6600",
            "editorBracketHighlight.foreground2": "#00000000",
            "editorBracketHighlight.foreground3": "#00aa88",
            "editorBracketHighlight.foreground4": null,
            "editorBracketHighlight.foreground5": "#8844dd",
            "editorBracketHighlight.foreground6": "#00000000",
        }),
        "vs-dark",
    );
    let palette = [
        Color::Rgb(255, 102, 0),
        Color::Rgb(0, 170, 136),
        Color::Rgb(136, 68, 221),
    ];
    for word_wrap in [false, true] {
        for selected in [false, true] {
            for (index, (_, foreground, _)) in
                rendered_nested_brackets(colors, word_wrap, selected, true)
                    .into_iter()
                    .enumerate()
            {
                let depth = index.min(23 - index);
                assert_eq!(
                    foreground,
                    palette[depth % palette.len()],
                    "bracket {index}: transparent and null slots must not consume nesting colors; word_wrap={word_wrap}, selected={selected}"
                );
            }
        }
    }
}

#[test]
fn vscode_color_empty_bracket_palette_retains_syntax_foregrounds() {
    let authored = (1..=6)
        .map(|index| {
            (
                format!("editorBracketHighlight.foreground{index}"),
                serde_json::json!("#00000000"),
            )
        })
        .collect::<Map<String, Value>>();
    let colors = imported_colors(Value::Object(authored), "vs-dark");
    for word_wrap in [false, true] {
        for selected in [false, true] {
            assert_eq!(
                rendered_nested_brackets(colors, word_wrap, selected, true),
                rendered_nested_brackets(colors, word_wrap, selected, false),
                "an empty palette must leave syntax coloring intact; word_wrap={word_wrap}, selected={selected}"
            );
        }
    }
}
