use super::*;
use ratatui::{Terminal, backend::TestBackend, buffer::Cell};

fn themed() -> App {
    let mut app = tests::seeded();
    // Exercise the real workbench-customization resolver. Deliberately distinct
    // surfaces reveal accidentally reusing editor or list colors in chat.
    app.settings_effective.workbench_color_customizations = serde_json::from_str(r##"{
        "editor.background":"#171519", "editor.foreground":"#f0e4dc",
        "sideBar.background":"#201824", "sideBar.foreground":"#eedaf5",
        "sideBarTitle.foreground":"#f7def7", "descriptionForeground":"#bdaec5",
        "focusBorder":"#deaceb", "input.background":"#302234",
        "input.foreground":"#f7e5cc", "input.placeholderForeground":"#c9a9bd",
        "input.border":"#c79ab7", "editorCursor.foreground":"#f1bfe8",
        "editorCursor.background":"#29132d",
        "button.background":"#68426a", "button.foreground":"#fff2de",
        "button.hoverBackground":"#88588a", "button.secondaryBackground":"#46324c",
        "button.secondaryForeground":"#e3d5f1", "button.secondaryHoverBackground":"#694766",
        "dropdown.background":"#362947", "dropdown.foreground":"#dbcdf8",
        "menu.background":"#293323", "menu.foreground":"#e4f0d4",
        "menu.selectionBackground":"#c8dfa0", "menu.selectionForeground":"#233316",
        "menu.border":"#92a777", "list.focusBackground":"#392943",
        "list.focusForeground":"#ead6fd", "list.hoverBackground":"#e0cde8",
        "list.hoverForeground":"#211b26", "editorWidget.background":"#1e3034",
        "editorWidget.foreground":"#d4f3f1", "editorWidget.border":"#759fa5",
        "editorHoverWidget.background":"#40322a", "editorHoverWidget.foreground":"#f8e4cb",
        "editorHoverWidget.border":"#dda877", "editorSuggestWidget.background":"#252840",
        "editorSuggestWidget.foreground":"#dadefc", "editorSuggestWidget.selectedBackground":"#bfc7f3",
        "editorSuggestWidget.selectedForeground":"#1c2140", "editorSuggestWidget.border":"#8191d2",
        "terminal.background":"#111e18", "terminal.foreground":"#adf3cb",
        "chat.requestBubbleBackground":"#39243d", "chat.requestBubbleHoverBackground":"#b392b9",
        "chat.avatarBackground":"#54395c", "chat.avatarForeground":"#ffe1aa",
        "chat.editedFileForeground":"#ecbdda", "chat.linesAddedForeground":"#b8e788",
        "chat.linesRemovedForeground":"#ffaab4", "editorGutter.addedBackground":"#b8e788",
        "editorGutter.deletedBackground":"#ffaab4", "diffEditor.insertedLineBackground":"#263322",
        "diffEditor.removedLineBackground":"#40222c", "textCodeBlock.background":"#211f30",
        "textPreformat.foreground":"#f3daab", "textLink.foreground":"#bdc5ff",
        "errorForeground":"#f6a4b5", "editorWarning.foreground":"#f0d278",
        "testing.iconPassed":"#b8e7b0"
    }"##).unwrap();
    assert!(app.set_theme_by_key("dark"));
    app.cursor_visible = false;
    app
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.text_input_regions.clear();
    terminal.draw(|f| draw(f, app, f.area())).unwrap();
}

fn position(terminal: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
    let buffer = terminal.backend().buffer();
    let index = buffer
        .content
        .windows(needle.chars().count())
        .position(|cells| cells.iter().map(|c| c.symbol()).collect::<String>() == needle)
        .unwrap_or_else(|| panic!("missing rendered text: {needle}"));
    (
        index as u16 % buffer.area.width,
        index as u16 / buffer.area.width,
    )
}

fn cell<'a>(terminal: &'a Terminal<TestBackend>, needle: &str) -> &'a Cell {
    terminal
        .backend()
        .buffer()
        .cell(position(terminal, needle))
        .unwrap()
}

fn pair(terminal: &Terminal<TestBackend>, needle: &str, foreground: Color, background: Color) {
    let cell = cell(terminal, needle);
    assert_eq!((cell.fg, cell.bg), (foreground, background), "{needle}");
}

fn capture(terminal: &Terminal<TestBackend>, name: &str) {
    let Some(directory) = std::env::var_os("VTCODE_CHAT_CAPTURE") else {
        return;
    };
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = buffer
        .content
        .iter()
        .map(|c| {
            serde_json::json!({
                "text":c.symbol(), "fg":format!("{:?}",c.fg), "bg":format!("{:?}",c.bg),
                "bold":c.modifier.contains(Modifier::BOLD)
            })
        })
        .collect();
    std::fs::write(std::path::Path::new(&directory).join(format!("chat-theme-{name}.json")),
        serde_json::to_vec(&serde_json::json!({"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
}

#[test]
fn picker_scrollbar_is_wide_inside_its_frame_and_uses_hover_theme_colors() {
    for theme in ["dark", "light", "high-contrast", "monokai"] {
        let mut app = tests::seeded();
        app.settings_effective.workbench_color_customizations = serde_json::from_value(serde_json::json!({
            "menu.background":"#223344", "menu.foreground":"#eef0f4",
            "menu.border":"#aabbcc", "menu.separatorBackground":"#778899",
            "scrollbar.background":"#00000020", "scrollbarSlider.background":"#ffffff30",
            "scrollbarSlider.hoverBackground":"#ffffff90"
        })).unwrap();
        app.set_theme_by_key(theme);
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        app.chat.picker = Some(Picker::Models);
        app.chat.choices = (0..40).map(|i| crate::app_acp::Choice {
            label: format!("Model choice {i}"), detail: "Model description".into(),
            action: ChatAction::AgentCommand(format!("model{i}")),
        }).collect();
        let mut terminal = Terminal::new(TestBackend::new(92, 34)).unwrap();
        render(&mut app, &mut terminal);
        let (_, y) = position(&terminal, "Model choice 0");
        let right = (0..92).rev().find(|x| terminal.backend().buffer()[(*x, y)].symbol() == "│").unwrap();
        let background = Color::Rgb(34, 51, 68);
        let (track, thumb) = app.colors.ui.scrollbar_colors(background, false, false);
        for x in right - 2..right {
            let cell = &terminal.backend().buffer()[(x, y)];
            assert_eq!((cell.symbol(), cell.fg, cell.bg), ("█", thumb, track));
            assert!(!app.chat.hits.iter().any(|hit| hit.area.contains((x, y).into())));
        }
        assert_eq!(terminal.backend().buffer()[(right, y)].fg, Color::Rgb(170, 187, 204));
        app.mouse_col = right - 1;
        app.mouse_row = y;
        render(&mut app, &mut terminal);
        let hover = app.colors.ui.scrollbar_colors(background, true, false).1;
        assert_ne!(thumb, hover);
        assert_eq!(terminal.backend().buffer()[(right - 1, y)].fg, hover);
        app.handle_chat_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: right - 1, row: y + 7, modifiers: crossterm::event::KeyModifiers::NONE,
        });
        app.handle_chat_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: right - 1, row: y + 7, modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(app.chat.picker, Some(Picker::Models));
        assert!(app.chat.picker_scroll > 0);
        render(&mut app, &mut terminal);
        let (_, bottom) = position(&terminal, "Model choice 39");
        assert!(bottom > y);
        for x in right - 2..right {
            assert_eq!(terminal.backend().buffer()[(x, bottom)].symbol(), "█");
        }
        capture(&terminal, &format!("wide-scrollbar-{theme}"));
        app.handle_chat_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: right - 1, row: bottom, modifiers: crossterm::event::KeyModifiers::NONE,
        });
        app.chat.choices.truncate(2);
        app.handle_chat_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
            column: right - 1, row: u16::MAX, modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(app.chat.picker_selected < 2);
        assert_eq!(app.chat.picker_scroll, 0);
        app.chat.choices.clear();
        app.handle_chat_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
            column: right - 1, row: u16::MAX, modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert_eq!(app.chat.picker_selected, 0);
        assert!(app.chat.scroll_drag.is_none());
        app.settings_effective.workbench_color_customizations = Default::default();
        app.set_theme_by_key(theme);
        app.chat.picker_selected = 0;
        app.chat.choices = (0..40).map(|i| crate::app_acp::Choice {
            label: format!("Model choice {i}"), detail: "Model description".into(),
            action: ChatAction::AgentCommand(format!("model{i}")),
        }).collect();
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        render(&mut app, &mut terminal);
        capture(&terminal, &format!("wide-scrollbar-native-{theme}"));
    }
}

#[test]
fn agent_and_conversation_hover_cards_preserve_opacity_on_the_covered_surface() {
    use crate::theme_workbench::ColorLayer;
    let mut app = themed();
    app.chat.session_mut().unwrap().title = "Conversation hover".into();
    let session = app.chat.selected.clone().unwrap();
    let hit_area = Rect::new(12, 1, 8, 1);
    app.mouse_col = hit_area.x;
    app.mouse_row = hit_area.y;
    let mut terminal = Terminal::new(TestBackend::new(88, 16)).unwrap();
    for owner in [Color::Rgb(20, 40, 60), Color::Rgb(50, 10, 65)] {
        for (alpha, border_alpha) in [(128, 128), (0, 128), (128, 0)] {
            app.settings_effective.workbench_color_customizations =
                serde_json::from_value(serde_json::json!({
                    "editorHoverWidget.background":format!("#285064{alpha:02x}"),
                    "editorHoverWidget.foreground":"#ffeeccE0",
                    "editorHoverWidget.border":format!("#dd99ff{border_alpha:02x}")
                }))
                .unwrap();
            assert!(app.set_theme_by_key("dark"));
            for (name, action, heading, detail) in [
                ("agent", ChatAction::Picker(Picker::Display), "Chat display", "Choose tool output"),
                (
                    "conversation",
                    ChatAction::Switch(session.clone()),
                    "Conversation hover",
                    "Ready",
                ),
            ] {
                app.chat.hits = vec![ChatHit {
                    area: hit_area,
                    action,
                }];
                terminal
                    .draw(|frame| {
                        frame.render_widget(
                            Block::default()
                                .style(Style::default().bg(owner).add_modifier(Modifier::ITALIC)),
                            frame.area(),
                        );
                        if name == "agent" {
                            header::tooltip(frame, &app);
                        } else {
                            tabs::draw_tooltip(frame, &app, frame.area());
                        }
                    })
                    .unwrap();
                let bg = ColorLayer::rgba(40, 80, 100, alpha).over(owner).unwrap();
                let fg = ColorLayer::rgba(255, 238, 204, 224).over(bg).unwrap();
                for label in [heading, detail] {
                    pair(&terminal, label, fg, bg);
                    assert!(!cell(&terminal, label).modifier.contains(Modifier::ITALIC));
                }
                assert!(cell(&terminal, heading).modifier.contains(Modifier::BOLD));
                let border = &terminal.backend().buffer()[(hit_area.x, hit_area.bottom())];
                assert_eq!(border.bg, bg);
                assert_eq!(
                    border.fg,
                    if border_alpha == 0 {
                        bg
                    } else {
                        ColorLayer::rgba(221, 153, 255, border_alpha)
                            .over(bg)
                            .unwrap()
                    }
                );
                crate::ui_control_visibility_tests::snapshot(
                    terminal.backend().buffer(),
                    &format!("chat-{name}-hover-opacity-{alpha}-{border_alpha}-on-{owner:?}"),
                );
            }
        }
    }
}

#[test]
fn acp_chat_theme_uses_input_button_menu_and_tooltip_pairs() {
    let mut app = themed();
    let mut terminal = Terminal::new(TestBackend::new(96, 48)).unwrap();
    app.chat.input = "Draft message".into();
    app.chat.cursor = app.chat.input.len();
    app.chat.session_mut().unwrap().usage = Some(p::UsageUpdate::new(53_000, 200_000));
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "Draft message",
        app.colors.fg_input,
        app.colors.bg_input,
    );
    pair(
        &terminal,
        "Workspace Large",
        Color::Rgb(219, 205, 248),
        Color::Rgb(54, 41, 71),
    );
    capture(&terminal, "controls");

    app.cursor_visible = true;
    app.chat.cursor = 0;
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "Draft message",
        Color::Rgb(48, 34, 52),
        Color::Rgb(247, 229, 204),
    );
    app.cursor_visible = false;
    app.chat.input.clear();
    app.chat.cursor = 0;
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "Ask anything",
        app.colors.fg_input_placeholder,
        app.colors.bg_input,
    );
    // Check the unfocused input border, independently of the panel border.
    app.active_pane = ActivePane::Editor;
    render(&mut app, &mut terminal);
    let (x, y) = position(&terminal, "Ask anything");
    assert_eq!(terminal.backend().buffer()[(x - 2, y - 1)].fg, Color::Rgb(199, 154, 183));
    app.active_pane = ActivePane::Chat;

    app.chat_action(ChatAction::Picker(Picker::Models));
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "Workspace Large",
        app.colors.ui.fg_menu_selected,
        app.colors.bg_menu_selected,
    );
    pair(
        &terminal,
        "Workspace Fast",
        app.colors.fg_menu,
        app.colors.bg_menu,
    );
    assert_eq!(
        cell(&terminal, "✓ Workspace Large").fg,
        app.colors.ui.fg_menu_selected
    );
    capture(&terminal, "menu");
    app.chat_action(ChatAction::DismissPicker);
    render(&mut app, &mut terminal);
    let badge = app.chat.context_usage_area.unwrap();
    app.mouse_col = badge.x;
    app.mouse_row = badge.y;
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "53,000 / 200,000 tokens",
        Color::Rgb(248, 228, 203),
        Color::Rgb(64, 50, 42),
    );
    capture(&terminal, "tooltip");

    // The welcome CTA is enabled without launching an agent process.
    app.mouse_col = 0;
    app.mouse_row = 0;
    app.chat.selected = None;
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "Connect an agent",
        Color::Rgb(255, 242, 222),
        app.colors.bg_sc_button,
    );
    let (x, y) = position(&terminal, "Connect an agent");
    app.mouse_col = x;
    app.mouse_row = y;
    render(&mut app, &mut terminal);
    assert_ne!(
        cell(&terminal, "Connect an agent").bg,
        app.colors.bg_sc_button
    );
    assert_eq!(
        cell(&terminal, "Connect an agent").fg,
        Color::Rgb(255, 242, 222)
    );
}

#[test]
fn acp_chat_theme_dropdown_hover_and_focus_outlines_override_the_idle_border() {
    use crate::theme_workbench::ColorLayer;
    for border in ["#aa66dd80", "#00000000"] {
        let mut app = themed();
        for (role, value) in [
            ("dropdown.border", border), ("toolbar.hoverOutline", "#ffcc00"),
            ("focusBorder", "#11cc77"),
        ] {
            app.settings_effective.workbench_color_customizations.insert(role.into(), value.into());
        }
        assert!(app.set_theme_by_key("high-contrast"));
        app.chat.picker = None;
        let area = Rect::new(2, 1, 20, 1);
        for state in 0..3 {
            let action = ChatAction::Picker(Picker::Models);
            app.chat.focus = (state == 2).then_some(action.clone());
            app.mouse_col = if state == 1 { area.x } else { u16::MAX };
            app.mouse_row = if state == 1 { area.y } else { u16::MAX };
            let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
            terminal.draw(|frame| {
                frame.render_widget(Paragraph::new("").style(Style::default().bg(app.colors.bg_sidebar)), frame.area());
                widgets::draw_button(frame, &mut app, area, "Model", action, true, widgets::ButtonKind::Dropdown);
            }).unwrap();
            let cell = &terminal.backend().buffer()[(area.x, area.y)];
            let expected = match state {
                1 => Some(Color::Rgb(255, 204, 0)),
                2 => Some(Color::Rgb(17, 204, 119)),
                _ if border == "#00000000" => None,
                _ => ColorLayer::rgba(170, 102, 221, 128).border(cell.bg),
            };
            assert_eq!(cell.modifier.contains(Modifier::UNDERLINED), expected.is_some());
            if let Some(outline) = expected { assert_eq!(cell.underline_color, outline); }
        }
    }
}

#[test]
fn acp_chat_theme_repaints_cached_messages_terminals_diffs_and_hovered_spans() {
    let mut app = themed();
    let mut terminal = Terminal::new(TestBackend::new(96, 60)).unwrap();
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "Add a friendly greeting",
        app.colors.ui.fg_sidebar,
        app.colors.ui.bg_chat_request,
    );
    pair(
        &terminal,
        "The greeting is ready",
        app.colors.ui.fg_sidebar,
        app.colors.bg_sidebar,
    );
    pair(
        &terminal,
        "●  You",
        app.colors.ui.chat_avatar_style(app.colors.ui.bg_chat_request).fg.unwrap(),
        app.colors.ui.chat_avatar_style(app.colors.ui.bg_chat_request).bg.unwrap(),
    );
    pair(
        &terminal,
        "test result: ok. 2 passed; 0 failed",
        app.colors.ui.fg_terminal,
        app.colors.ui.bg_terminal,
    );
    assert_eq!(
        cell(&terminal, "↗ greeting.rs").fg,
        app.colors.ui.fg_chat_edited_file
    );
    assert_eq!(cell(&terminal, "+1").fg, app.colors.ui.fg_chat_added);
    assert_eq!(cell(&terminal, "−1").fg, app.colors.ui.fg_chat_removed);
    assert_eq!(
        cell(&terminal, "+ Hello, world!").bg,
        app.colors.bg_diff_added
    );
    assert_eq!(cell(&terminal, "- Hello").bg, app.colors.bg_diff_removed);
    capture(&terminal, "transcript");

    let revision = app.chat.session().unwrap().revision;
    let cached = app.chat.cache.entries[0].2[0].clone();
    app.colors.ui.fg_terminal = Color::Rgb(249, 214, 176);
    app.colors.ui.bg_terminal = Color::Rgb(42, 28, 15);
    app.colors.ui.bg_chat_request = Color::Rgb(24, 45, 51);
    render(&mut app, &mut terminal);
    assert_eq!(app.chat.session().unwrap().revision, revision);
    assert!(!Arc::ptr_eq(&cached, &app.chat.cache.entries[0].2[0]));
    pair(
        &terminal,
        "test result: ok. 2 passed; 0 failed",
        app.colors.ui.fg_terminal,
        app.colors.ui.bg_terminal,
    );
    assert_eq!(
        cell(&terminal, "Add a friendly greeting").bg,
        app.colors.ui.bg_chat_request
    );

    let (x, y) = position(&terminal, "↗ greeting.rs");
    app.mouse_col = x;
    app.mouse_row = y;
    render(&mut app, &mut terminal);
    // An interactive row's already styled spans must all inherit the hover pair.
    pair(
        &terminal,
        "↗ greeting.rs",
        Color::Rgb(33, 27, 38),
        Color::Rgb(224, 205, 232),
    );
    pair(
        &terminal,
        "+1",
        Color::Rgb(33, 27, 38),
        Color::Rgb(224, 205, 232),
    );
    pair(
        &terminal,
        "−1",
        Color::Rgb(33, 27, 38),
        Color::Rgb(224, 205, 232),
    );
    capture(&terminal, "hover");
    app.mouse_col = 0;
    app.mouse_row = 0;
    app.settings_effective
        .workbench_color_customizations
        .clear();
    app.set_theme_by_key("light");
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "The greeting is ready",
        app.colors.ui.fg_sidebar,
        app.colors.bg_sidebar,
    );
    pair(
        &terminal,
        "test result: ok. 2 passed; 0 failed",
        app.colors.ui.fg_terminal,
        app.colors.ui.bg_terminal,
    );
}

#[test]
fn request_bubble_text_code_avatars_and_hover_preserve_their_own_opacity() {
    use crate::theme_workbench::ColorLayer;
    let mut app = themed();
    for (id, value) in [
        ("sideBar.background", "#15202b"), ("sideBar.foreground", "#ffeeddc0"),
        ("chat.requestBubbleBackground", "#66336680"),
        ("chat.requestBubbleHoverBackground", "#57773380"),
        ("chat.avatarBackground", "#2c4c5c80"), ("chat.avatarForeground", "#ffeecce0"),
        ("textPreformat.background", "#55663380"), ("textPreformat.foreground", "#ffffdde0"),
        ("textCodeBlock.background", "#122e4a80"), ("list.hoverForeground", "#eeffbbd0"),
    ] { app.settings_effective.workbench_color_customizations.insert(id.into(), value.into()); }
    assert!(app.set_theme_by_key("dark"));
    app.mouse_col = u16::MAX;
    app.mouse_row = u16::MAX;
    let session = app.chat.session_mut().unwrap();
    *session = Session::new(session.id.clone());
    session.push(ChatEntry::User("User prose with `request_code`\n\n```text\nrequest_block\n```".into()));
    session.push(ChatEntry::Agent("Agent prose with `agent_code`\n\n```text\nagent_block\n```".into()));
    let revision = session.revision;
    let mut terminal = Terminal::new(TestBackend::new(96, 48)).unwrap();
    render(&mut app, &mut terminal);
    let sidebar = Color::Rgb(21, 32, 43);
    let bubble = ColorLayer::rgba(102, 51, 102, 128).over(sidebar).unwrap();
    let agent_avatar = format!("◇  {}", app.chat.name());
    for (prose, avatar, code, block, surface) in [
        ("User prose", "●  You", "request_code", "request_block", bubble),
        ("Agent prose", agent_avatar.as_str(), "agent_code", "agent_block", sidebar),
    ] {
        pair(&terminal, prose, ColorLayer::rgba(255, 238, 221, 192).over(surface).unwrap(), surface);
        let avatar_bg = ColorLayer::rgba(44, 76, 92, 128).over(surface).unwrap();
        pair(&terminal, avatar, ColorLayer::rgba(255, 238, 204, 224).over(avatar_bg).unwrap(), avatar_bg);
        let code_bg = ColorLayer::rgba(85, 102, 51, 128).over(surface).unwrap();
        pair(&terminal, code, ColorLayer::rgba(255, 255, 221, 224).over(code_bg).unwrap(), code_bg);
        assert_eq!(cell(&terminal, block).bg, ColorLayer::rgba(18, 46, 74, 128).over(surface).unwrap());
    }
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), "chat bubble and avatar opacity");

    let before = app.chat.cache.entries[0].2.clone();
    // Two raw roles can yield the same cached foreground on the sidebar while
    // producing different colors over the bubble. That must invalidate layout.
    app.colors.ui.sidebar_foreground = ColorLayer::opaque(app.colors.ui.fg_sidebar);
    render(&mut app, &mut terminal);
    assert_eq!(app.chat.session().unwrap().revision, revision);
    assert!(!Arc::ptr_eq(&before[0], &app.chat.cache.entries[0].2[0]));
    pair(&terminal, "User prose", app.colors.ui.fg_sidebar, bubble);
    pair(&terminal, "Agent prose", app.colors.ui.fg_sidebar, sidebar);

    (app.mouse_col, app.mouse_row) = position(&terminal, "User prose");
    render(&mut app, &mut terminal);
    let hover = ColorLayer::rgba(87, 119, 51, 128).over(bubble).unwrap();
    let hover_fg = ColorLayer::rgba(238, 255, 187, 208).over(hover).unwrap();
    for label in ["User prose", "●  You", "request_code", "request_block"] {
        pair(&terminal, label, hover_fg, hover);
    }
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), "chat bubble hover opacity");

    app.colors.ui.chat_request_hover_background = ColorLayer::rgba(0, 0, 0, 0);
    render(&mut app, &mut terminal);
    assert_eq!(cell(&terminal, "User prose").bg, bubble);
}

#[test]
fn acp_chat_theme_syntax_registry_changes_recolor_existing_fenced_code() {
    let mut app = themed();
    let session = app.chat.session_mut().unwrap();
    *session = Session::new(session.id.clone());
    session.push(ChatEntry::Agent("```rust\nlet answer = 42;\n```".into()));
    let revision = session.revision;
    let mut terminal = Terminal::new(TestBackend::new(70, 30)).unwrap();
    app.syntax_theme = "chat-test-syntax".into();
    for color in ["#F2BE76", "#ABE7FF"] {
        let mut theme = syntect::highlighting::Theme::default();
        theme.settings.foreground = crate::vscode_theme::parse_syntect_color(color);
        app.highlighter.register_theme(&app.syntax_theme, theme);
        render(&mut app, &mut terminal);
        pair(
            &terminal,
            "let answer",
            crate::app_theme::hex_to_color(color),
            app.colors.bg_md_code,
        );
        assert_eq!(app.chat.session().unwrap().revision, revision);
    }
    capture(&terminal, "syntax");
}

#[test]
fn acp_chat_theme_uses_suggestion_and_permission_surfaces() {
    let root = tempfile::tempdir().unwrap();
    let mut app = themed();
    app.explorer.root = root.path().to_path_buf();
    app.chat.input = "@".into();
    app.chat.cursor = 1;
    app.refresh_chat_file_completion();
    let completion = app.chat.files.completion.as_mut().unwrap();
    completion.paths = vec![root.path().join("first.rs"), root.path().join("second.rs")];
    completion.searching = false;
    let mut terminal = Terminal::new(TestBackend::new(96, 44)).unwrap();
    render(&mut app, &mut terminal);
    pair(
        &terminal,
        "first.rs",
        Color::Rgb(28, 33, 64),
        Color::Rgb(191, 199, 243),
    );
    pair(
        &terminal,
        "second.rs",
        Color::Rgb(218, 222, 252),
        Color::Rgb(37, 40, 64),
    );
    assert_eq!(cell(&terminal, "Add files").fg, Color::Rgb(218, 222, 252));
    capture(&terminal, "suggestions");
    app.chat.files.dismiss();
    app.apply_acp_event(vtcode::acp::Event::Permission {
        ticket: 0,
        id: serde_json::from_value(serde_json::json!(17)).unwrap(),
        request: serde_json::from_value(serde_json::json!({"sessionId":"ui-session",
            "toolCall":{"toolCallId":"check","title":"Run checks"},
            "options":[{"optionId":"once","name":"Allow once","kind":"allow_once"}]}))
        .unwrap(),
    });
    render(&mut app, &mut terminal);
    // The tool title also appears in the transcript; inspect the permission card.
    let option = app
        .chat
        .hits
        .iter()
        .find(|h| matches!(h.action, ChatAction::Permission(_, _, Some(_))))
        .unwrap()
        .area;
    let title = terminal
        .backend()
        .buffer()
        .cell((option.x, option.y - 1))
        .unwrap();
    assert_eq!(
        (title.fg, title.bg),
        (app.colors.ui.fg_widget, app.colors.ui.bg_widget)
    );
    pair(
        &terminal,
        "Allow once",
        Color::Rgb(227, 213, 241),
        app.colors.bg_sc_button_secondary,
    );
    capture(&terminal, "permission");
}

#[test]
fn acp_chat_theme_fallback_palette_changes_refresh_cached_prose() {
    let mut app = tests::seeded();
    app.colors.ui.preserve_theme_colors = false;
    app.colors.bg_sidebar = Color::Rgb(40, 40, 40);
    app.colors.ui.fg_sidebar = app.colors.bg_sidebar;
    app.colors.ui.sidebar_foreground = crate::theme_workbench::ColorLayer::opaque(app.colors.bg_sidebar);
    app.colors.ui.contrast_palette = [Color::Rgb(200, 210, 220); 14];
    let mut terminal = Terminal::new(TestBackend::new(110, 35)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut app, frame.area()))
        .unwrap();
    let prose = |app: &App| {
        app.chat
            .cache
            .rows
            .iter()
            .find(|row| {
                row.line
                    .spans
                    .iter()
                    .any(|span| span.content.contains("I'll update the greeting"))
            })
            .unwrap()
            .clone()
    };
    let before = prose(&app);
    assert!(
        before
            .line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(Color::Rgb(200, 210, 220)))
    );
    // No entry, owning surface, prose color, or syntax theme changed. Only the
    // palette used for a low-contrast fallback changed.
    app.colors.ui.contrast_palette = [Color::Rgb(230, 240, 250); 14];
    terminal
        .draw(|frame| draw(frame, &mut app, frame.area()))
        .unwrap();
    let after = prose(&app);
    assert!(!Arc::ptr_eq(&before, &after));
    assert!(
        after
            .line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(Color::Rgb(230, 240, 250)))
    );
}
