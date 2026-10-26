use super::*;

#[test]
fn tool_links_use_schema_locations_and_hide_remote_view_key_roots() {
    let app = App::test_new();
    let key = std::path::PathBuf::from(if cfg!(windows) { "C:/" } else { "/" }).join(format!("{}vtcode-remote-fixture", char::from(0u8))).join("project/dir/colon%3afile.rs");
    let tool: p::ToolCall = serde_json::from_value(serde_json::json!({"toolCallId":"view", "title":"Read file", "kind":"read", "status":"completed", "locations":[{"path":key,"line":7}], "rawInput":{"path":"/node/original.rs", "view_range":[3,4]}})).unwrap();
    assert_eq!(tools::location(&tool), Some((key.clone(), Some(7))));
    let mut rows = Vec::new();
    tools::body(&mut rows, &tool, 100, &app.colors, true, None);
    let text: String = rows.iter().map(|row| row.line.to_string()).collect();
    assert!(text.contains("dir/colon:file.rs:8"), "{text}");
    assert!(!text.contains(char::from(0u8)) && !text.contains("vtcode-remote") && !text.contains("/node/original.rs"));
    assert!(rows.iter().any(|row| row.action == Some(ChatAction::OpenFile(key.clone(), Some(7)))));
}
use crate::app::{Tab, TabDropZone, TabKind};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::empty(),
        },
    );
}
fn paint(app: &mut App, terminal: &mut Terminal<TestBackend>, full: bool) {
    app.text_input_regions.clear();
    terminal
        .draw(|f| {
            if full {
                crate::ui::draw(f, app);
            } else {
                draw(f, app, f.area());
            }
        })
        .unwrap();
}
fn text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect()
}
fn position(terminal: &Terminal<TestBackend>, needle: &str) -> (u16, u16) {
    let b = terminal.backend().buffer();
    for y in 0..b.area.height {
        for x in 0..b.area.width.saturating_sub(needle.width() as u16) {
            let s: String = (x..x + needle.width() as u16)
                .map(|x| b[(x, y)].symbol())
                .collect();
            if s == needle {
                return (x, y);
            }
        }
    }
    panic!("Missing {needle}");
}
fn empty_chat() -> App {
    let mut app = tests::seeded();
    let s = app.chat.session_mut().unwrap();
    *s = Session::new(s.id.clone());
    app
}

#[test]
fn chat_scrollbar_captures_drag_outside_chat_and_reaches_both_ends() {
    let mut app = empty_chat();
    for i in 0..1000 {
        app.chat
            .session_mut()
            .unwrap()
            .push(ChatEntry::Agent(format!("Message {i}")));
    }
    let mut terminal = Terminal::new(TestBackend::new(90, 35)).unwrap();
    paint(&mut app, &mut terminal, false);
    let bar = app
        .chat
        .scrollbars
        .iter()
        .find(|b| b.terminal.is_none())
        .unwrap()
        .clone();
    assert_eq!(bar.area.width, 2);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.right() - 1,
        bar.area.y + bar.area.height / 2,
    );
    assert!(app.chat.scroll > 0 && app.chat.scroll < app.chat.max_scroll);
    assert!(!app.chat.follow);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 0);
    assert_eq!(app.chat.scroll, 0);
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 200);
    assert_eq!(app.chat.scroll, app.chat.max_scroll);
    assert!(app.chat.follow);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 200);
    assert!(app.chat.scroll_drag.is_none());
}

#[test]
fn chat_middle_click_closes_tab_without_stopping_its_session() {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    app.chat.session_mut().unwrap().busy = true;
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    paint(&mut app, &mut terminal, false);
    let area = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == ChatAction::Switch(id.clone()))
        .unwrap()
        .area;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Middle),
        area.x + 3,
        area.y,
    );
    assert!(!app.chat.open_tabs.contains(&id));
    assert!(app.chat.sessions[&id].busy);
}

#[test]
fn chat_tool_detail_modes_hide_preview_keep_links_and_allow_individual_overrides() {
    let mut app = empty_chat();
    let source: String = (0..410).map(|i| format!("let line_{i} = {i};\n")).collect();
    app.chat.session_mut().unwrap().apply(serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"view", "title":"Viewing src/example.rs", "kind":"read", "status":"completed",
        "rawInput":{"path":"src/example.rs"}, "rawOutput":{"content":source}
    })).unwrap());
    let mut terminal = Terminal::new(TestBackend::new(96, 40)).unwrap();
    paint(&mut app, &mut terminal, false);
    assert!(text(&terminal).contains("line_0"));
    assert!(!text(&terminal).contains("↗ src/example.rs"));
    let link = ChatAction::OpenFile("src/example.rs".into(), None);
    assert!(app.chat.hits.iter().any(|h| h.action == link));
    for detail in [OutputDetail::Minimal, OutputDetail::Full] {
        app.chat.output_detail = detail;
        paint(&mut app, &mut terminal, false);
        let rows: String = app
            .chat
            .cache
            .rows
            .iter()
            .map(|r| r.line.to_string())
            .collect();
        assert_eq!(rows.contains("line_409"), detail == OutputDetail::Full);
        assert_eq!(
            rows.contains("↗ src/example.rs"),
            detail == OutputDetail::Full
        );
    }
    app.chat_action(ChatAction::ToggleEntry(0));
    paint(&mut app, &mut terminal, false);
    assert!(!app.chat.entry_expanded(0));
    app.chat_action(ChatAction::ToggleEntry(0));
    assert!(app.chat.entry_expanded(0));
    app.chat.output_detail = OutputDetail::Minimal;
    app.chat.expanded.clear();
    paint(&mut app, &mut terminal, false);
    presentation_tests::capture(&terminal, "controls-minimal");
}

#[test]
fn chat_loading_conversation_is_centered_and_clears_when_ready() {
    let mut app = empty_chat();
    app.chat.control_busy = true;
    app.chat.status = "Creating conversation…".into();
    let mut terminal = Terminal::new(TestBackend::new(90, 40)).unwrap();
    paint(&mut app, &mut terminal, false);
    let (_, y) = position(&terminal, "◌ Creating conversation…");
    let area = app.chat.transcript_area.unwrap();
    assert_eq!(y, area.y + area.height / 2);
    presentation_tests::capture(&terminal, "controls-loading");
    app.apply_acp_event(vtcode::acp::Event::SessionOpened {
        session_id: p::SessionId::new("ready"),
        modes: None,
        config_options: vec![],
        loaded: false,
    });
    paint(&mut app, &mut terminal, false);
    assert!(!text(&terminal).contains("Creating conversation"));
}

#[test]
fn chat_terminal_chrome_is_outside_output_and_ansi_colors_follow_the_terminal_theme() {
    let mut app = empty_chat();
    app.chat.session_mut().unwrap().apply(serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"color", "title":"Check changes", "kind":"execute", "status":"in_progress",
        "rawInput":{"command":"git diff --color=always"},
        "rawOutput":{"stdout":"\u{1b}[31mremoved\u{1b}[0m\n\u{1b}[1;38;2;10;200;90madded\u{1b}[0m\n@@ diff header\n+new line\n-old line\n"}
    })).unwrap());
    app.chat.session_mut().unwrap().busy = true;
    app.chat.expanded.insert(0);
    let mut terminal = Terminal::new(TestBackend::new(84, 40)).unwrap();
    for theme in ["dark", "light", "monokai"] {
        app.set_theme_by_key(theme);
        paint(&mut app, &mut terminal, false);
        let command = position(&terminal, "$ git diff");
        let controls = app
            .chat
            .hits
            .iter()
            .find(|h| h.action == ChatAction::ToggleEntry(0))
            .unwrap()
            .area;
        assert!(controls.y < command.1);
        assert!(position(&terminal, "Running").1 < command.1);
        let red = position(&terminal, "removed");
        assert_eq!(terminal.backend().buffer()[red].fg, app.colors.ui.ansi[1]);
        let rgb = position(&terminal, "added");
        let cell = &terminal.backend().buffer()[rgb];
        assert_eq!(cell.fg, Color::Rgb(10, 200, 90));
        assert!(cell.modifier.contains(Modifier::BOLD));
        assert_eq!(
            terminal.backend().buffer()[command].bg,
            app.colors.ui.bg_terminal
        );
        assert_ne!(
            terminal.backend().buffer()[(controls.x, controls.y)].bg,
            app.colors.ui.bg_terminal
        );
        presentation_tests::capture(&terminal, &format!("controls-terminal-{theme}"));
    }
}

#[test]
fn chat_tabs_drag_to_editor_splits_and_back_with_draft_and_session_intact() {
    for edge in [
        TabDropZone::Left,
        TabDropZone::Right,
        TabDropZone::Top,
        TabDropZone::Bottom,
    ] {
        let mut app = empty_chat();
        app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
        app.active_tab = 0;
        app.show_explorer = false;
        app.show_panel = false;
        app.terminal_sidebar_revealed = false;
        app.chat.input = "keep this draft".into();
        app.chat.cursor = app.chat.input.len();
        let id = app.chat.selected.clone().unwrap();
        app.chat
            .session_mut()
            .unwrap()
            .push(ChatEntry::Agent("CONVERSATION_SURVIVES_MOVE".into()));
        let mut terminal = Terminal::new(TestBackend::new(160, 55)).unwrap();
        paint(&mut app, &mut terminal, true);
        let tab = app
            .chat
            .hits
            .iter()
            .find(|h| h.action == ChatAction::Switch(id.clone()))
            .unwrap()
            .area;
        let editor = app.editor_area.unwrap();
        let (x, y) = match edge {
            TabDropZone::Left => (editor.x + 1, editor.y + editor.height / 2),
            TabDropZone::Right => (editor.right() - 2, editor.y + editor.height / 2),
            TabDropZone::Top => (editor.x + editor.width / 2, editor.y + 2),
            TabDropZone::Bottom => (editor.x + editor.width / 2, editor.bottom() - 2),
        };
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            tab.x + 3,
            tab.y,
        );
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), x, y);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, y);
        paint(&mut app, &mut terminal, true);
        assert!(matches!(app.tabs[app.active_tab].kind, TabKind::Chat(_)));
        assert!(app.split_active());
        assert_eq!(app.chat.input, "keep this draft");
        assert!(text(&terminal).contains("CONVERSATION_SURVIVES_MOVE"));
        assert!(app.chat.layout.surfaces.iter().any(|s| s.key.is_some()));
        presentation_tests::capture(&terminal, "controls-chat-editor-split");
        let tabs = app.tab_bar_area.unwrap();
        let tab_x=crate::ui_chrome_tabs::TabBarHits::new(&app,tabs).tabs[0].1.x;
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            tab_x + 3,
            tabs.y,
        );
        let body = app.workbench_body_area.unwrap();
        let point = (body.right() - 1, body.y + body.height / 2);
        mouse(
            &mut app,
            MouseEventKind::Drag(MouseButton::Left),
            point.0,
            point.1,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            point.0,
            point.1,
        );
        paint(&mut app, &mut terminal, true);
        assert!(app.chat.open_tabs.contains(&id));
        assert!(app.chat.layout.editors.is_empty());
        assert_eq!(app.chat.input, "keep this draft");
        assert!(text(&terminal).contains("CONVERSATION_SURVIVES_MOVE"));
    }
}

#[test]
fn chat_separate_editor_panes_keep_inputs_isolated_and_middle_close_only_one_view() {
    let mut app = empty_chat();
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    app.show_explorer = false;
    app.show_panel = false;
    app.terminal_sidebar_revealed = false;
    let first = app.chat.selected.clone().unwrap();
    app.chat.input = "FIRST_DRAFT".into();
    app.chat.cursor = app.chat.input.len();
    app.chat.session_mut().unwrap().busy = true;
    let second = p::SessionId::new("second");
    app.apply_acp_event(vtcode::acp::Event::SessionOpened {
        session_id: second.clone(),
        modes: None,
        config_options: vec![],
        loaded: false,
    });
    app.chat.input = "SECOND_DRAFT".into();
    app.chat.cursor = app.chat.input.len();
    app.place_chat_in_editor(first.clone(), 0, Some(TabDropZone::Right));
    app.place_chat_in_editor(second.clone(), 0, Some(TabDropZone::Bottom));
    let mut terminal = Terminal::new(TestBackend::new(180, 60)).unwrap();
    paint(&mut app, &mut terminal, true);
    assert!(text(&terminal).contains("FIRST_DRAFT"));
    assert!(text(&terminal).contains("SECOND_DRAFT"));
    assert_eq!(app.chat.selected.as_ref(), Some(&second));
    let (x, y) = position(&terminal, "FIRST_DRAFT");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x + 5, y);
    assert_eq!(app.chat.selected.as_ref(), Some(&first));
    assert_eq!(app.chat.input, "FIRST_DRAFT");
    app.paste_into_focused_text_input("_EDIT_");
    paint(&mut app, &mut terminal, true);
    assert!(app.chat.input.contains("_EDIT_"));
    let tabs = app.tab_bar_area.unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Middle),
        tabs.x + 3,
        tabs.y,
    );
    paint(&mut app, &mut terminal, true);
    assert!(app.chat.sessions[&first].busy);
    assert!(!app.chat.layout.editors.values().any(|view| view.session == first));
    assert!(app.activate_editor_chat(&second));
    assert_eq!(app.chat.input, "SECOND_DRAFT");
    assert!(app.chat.sessions.contains_key(&first));
}

#[test]
fn chat_workbench_tab_can_split_inside_its_own_conversation_pane() {
    let mut app = empty_chat();
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    app.show_explorer = false;
    app.show_panel = false;
    app.terminal_sidebar_revealed = false;
    let id = app.chat.selected.clone().unwrap();
    app.chat.input = "DRAFT_BEFORE_SPLIT".into();
    app.chat.session_mut().unwrap().title = "PLACED_CHAT".into();
    app.place_chat_in_editor(id.clone(), 0, None);
    let mut terminal = Terminal::new(TestBackend::new(140, 55)).unwrap();
    paint(&mut app, &mut terminal, true);
    let tabs = app.tab_bar_area.unwrap();
    let x = position(&terminal, "PLACED_CHAT").0;
    let pane = app.editor_area.unwrap();
    let to = (pane.x + pane.width / 2, pane.bottom() - 2);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, tabs.y);
    assert!(app.dragged_chat_key().is_some());
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        to.0,
        to.1,
    );
    assert!(app.tab_drop_zone == Some(TabDropZone::Bottom));
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), to.0, to.1);
    paint(&mut app, &mut terminal, true);
    assert!(app.split_active());
    assert_eq!(app.chat.selected.as_ref(), Some(&id));
    assert_eq!(app.chat.input, "DRAFT_BEFORE_SPLIT");
    assert_eq!(app.chat.layout.editors.len(), 1);
}

#[test]
fn chat_workbench_switch_restores_draft_after_background_terminal_exits() {
    let mut app = empty_chat();
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    let first = app.chat.selected.clone().unwrap();
    app.place_chat_in_editor(first.clone(), 0, None);
    let first_key = app.chat.layout.active.unwrap();
    app.chat.input = "PRESERVED_CHAT_DRAFT".into();
    app.chat
        .session_mut()
        .unwrap()
        .terminal(vtcode::acp::TerminalSnapshot {
            session_id: first.clone(),
            id: "term".into(),
            command: "read input".into(),
            output: String::new(),
            truncated: false,
            exit_status: None,
            released: false,
            cwd: app.explorer.root.clone(),
            history: None,
            history_version: 0,
            killed: false,
            error: None,
        });
    app.chat_action(ChatAction::Terminal("term".into()));
    assert_eq!(app.chat.terminal_input.as_deref(), Some("term"));
    let second = p::SessionId::new("second");
    app.apply_acp_event(vtcode::acp::Event::SessionOpened {
        session_id: second.clone(),
        modes: None,
        config_options: vec![],
        loaded: false,
    });
    app.place_chat_in_editor(second, 0, None);
    app.chat
        .sessions
        .get_mut(&first)
        .unwrap()
        .terminals
        .get_mut("term")
        .unwrap()
        .killed = true;
    app.active_tab = app
        .tabs
        .iter()
        .position(|tab| tab.kind == TabKind::Chat(first_key))
        .unwrap();
    app.active_pane = ActivePane::Editor;
    app.chat.focus = Some(ChatAction::ToggleEntry(200));
    let mut terminal = Terminal::new(TestBackend::new(140, 50)).unwrap();
    paint(&mut app, &mut terminal, true);
    assert_eq!(app.chat.selected.as_ref(), Some(&first));
    assert!(app.chat.terminal_input.is_none());
    assert_eq!(app.chat.input, "PRESERVED_CHAT_DRAFT");
    assert!(app.chat.focus.is_none());
}

#[test]
fn chat_failed_new_agent_preserves_other_agent_placements_and_keeps_the_dock_visible() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = empty_chat();
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    let file_key = app.tabs[0].editor.navigation_id;
    app.explorer.root = directory.path().into();
    app.place_chat_in_editor(app.chat.selected.clone().unwrap(), 0, None);
    let original = app.chat.layout.active.unwrap();
    let original_agent = app.chat.active_agent;
    app.settings_effective.acp_agents = Some(vec![vtcode::acp::AgentConfig::new(
        "Unavailable agent",
        directory.path().join("missing-agent.exe").to_string_lossy(),
    )]);
    app.chat_action(ChatAction::Connect(0));
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.chat.error.is_none() && Instant::now() < deadline {
        app.poll_acp();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(app.chat.error.is_some());
    assert!(app.chat.layout.dock_visible);
    assert_eq!(app.chat.layout.editors.len(), 1);
    assert_eq!(app.chat.layout.editors[&original].agent, original_agent);
    let mut terminal = Terminal::new(TestBackend::new(140, 50)).unwrap();
    paint(&mut app, &mut terminal, true);
    assert!(app.chat.area.is_some());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[0].editor.navigation_id, file_key);
}

#[test]
fn chat_tool_output_button_persists_its_default_and_reset_restores_compact() {
    let root = tempfile::tempdir().unwrap();
    let mut app = empty_chat();
    app.settings_global_path = root.path().join("settings.json");
    app.settings_workspace_path = root.path().join("workspace-settings.json");
    app.settings_scope = crate::settings::SettingsScope::Global;
    app.settings_persistence_enabled = true;
    app.chat.output_detail = OutputDetail::Compact;
    app.chat_action(ChatAction::OutputDetail);
    assert_eq!(app.chat.output_detail, OutputDetail::Full);
    let saved: crate::settings::Settings =
        serde_json::from_slice(&std::fs::read(&app.settings_global_path).unwrap()).unwrap();
    assert_eq!(saved.chat_output_detail.as_deref(), Some("full"));
    app.chat_action(ChatAction::OutputDetail);
    assert_eq!(app.chat.output_detail, OutputDetail::Minimal);
    app.apply_resolved_settings(&crate::settings::Settings::default());
    assert_eq!(app.chat.output_detail, OutputDetail::Compact);
}

#[test]
fn chat_display_preferences_preserve_editor_syntax_highlights() {
    use crate::editor::Editor;
    use crate::settings::SettingsScope;
    use std::rc::Rc;

    for scope in [SettingsScope::Global, SettingsScope::Workspace] {
        let root = tempfile::tempdir().unwrap();
        let mut app = empty_chat();
        app.settings_global_path = root.path().join("settings.json");
        app.settings_workspace_path = root.path().join("workspace-settings.json");
        app.settings_scope = scope;
        app.settings_persistence_enabled = true;
        app.apply_effective_settings();

        let path = root.path().join("highlight.rs");
        let source = "let answer = 42;\n";
        let mut editor = Editor::test_with_text(source);
        editor.file_path = Some(path.clone());
        let rope = editor.rope.clone();
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        let job = app
            .highlighter
            .prepare_job(
                &rope,
                &path,
                0,
                None,
                0..rope.len_lines(),
                None,
                &app.syntax_theme,
            )
            .unwrap();
        let (syntaxes, themes) = app.highlighter.shared_sets();
        assert!(
            app.highlighter
                .apply_batch(job.run(&rope, &syntaxes, &themes, || false))
        );
        app.editor_mut().unwrap().syntax_dirty_from = None;
        app.editor_mut().unwrap().syntax_dirty_through = None;
        app.refresh_visible_highlights_async();
        let before = app
            .highlighter
            .highlighted_line_if_cached(source, 0, Some(&path), None, &app.syntax_theme)
            .unwrap();
        let generation = app.highlighter.registry_generation();
        let request = app.visible_highlight_key.clone();
        let pending = crate::background::CancellationToken::new();
        app.visible_highlight_cancel_token = Some(pending.clone());

        for action in [
            ChatAction::SetOutputDetail(OutputDetail::Minimal),
            ChatAction::SetThinkingDetail(true),
            ChatAction::OutputDetail,
            ChatAction::ThinkingDetail,
            ChatAction::OutputDetail,
            ChatAction::ThinkingDetail,
            ChatAction::OutputDetail,
        ] {
            app.chat_action(action);
            let after = app
                .highlighter
                .highlighted_line_if_cached(source, 0, Some(&path), None, &app.syntax_theme)
                .expect("chat display changes must not discard the editor's colored text");
            assert!(Rc::ptr_eq(&before, &after));
            assert_eq!(app.highlighter.registry_generation(), generation);
            assert_eq!(app.visible_highlight_key, request);
            assert!(!pending.is_cancelled(), "chat toggles must not cancel editor work");
            let saved = crate::settings::load_from_file(match scope {
                SettingsScope::Global => &app.settings_global_path,
                SettingsScope::Workspace => &app.settings_workspace_path,
            });
            assert_eq!(
                saved.chat_output_detail.as_deref(),
                Some(app.chat.output_detail.label().to_lowercase().as_str())
            );
            assert_eq!(
                saved.chat_thinking_expanded.unwrap_or(false),
                app.chat.thinking_expanded
            );
        }
    }
}

#[test]
fn chat_terminal_scrollbar_moves_only_that_terminal() {
    let mut app = empty_chat();
    let output: String = (0..100)
        .map(|i| format!("\x1b[32mline {i}\x1b[0m\n"))
        .collect();
    app.chat.session_mut().unwrap().apply(serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"scroll", "title":"long command", "kind":"execute", "status":"completed", "rawOutput":{"stdout":output}
    })).unwrap());
    app.chat.expanded.insert(0);
    let mut terminal = Terminal::new(TestBackend::new(84, 40)).unwrap();
    paint(&mut app, &mut terminal, false);
    let bar = app
        .chat
        .scrollbars
        .iter()
        .find(|bar| bar.terminal.is_some())
        .unwrap()
        .clone();
    let outer = app.chat.scroll;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.area.y,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        bar.area.x,
        bar.area.bottom() - 1,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        bar.area.x,
        bar.area.bottom() - 1,
    );
    paint(&mut app, &mut terminal, false);
    assert_eq!(app.chat.scroll, outer);
    assert!(text(&terminal).contains("line 99"));
    let last = position(&terminal, "line 99");
    assert_eq!(terminal.backend().buffer()[last].fg, app.colors.ui.ansi[2]);
}
#[test]
fn chat_yolo_toggle_is_compact_stable_and_available_during_a_turn() {
    for (theme, width) in [
        ("dark", 110),
        ("light", 64),
        ("high-contrast", 45),
        ("dark", 24),
    ] {
        let mut app = empty_chat();
        app.set_theme_by_key(theme);
        app.chat.rollback.enabled = true;
        app.chat
            .session_mut()
            .unwrap()
            .begin_prompt("Working".into());
        app.mouse_col = 0;
        app.mouse_row = 29;
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        paint(&mut app, &mut terminal, false);
        let action_area = |app: &App| {
            app.chat
                .hits
                .iter()
                .find(|hit| hit.action == ChatAction::Yolo)
                .unwrap()
                .area
        };
        let off = action_area(&app);
        assert!(!app.chat.yolo_enabled());
        let area = app.chat.area.unwrap();
        let controls = header_layout(
            &app,
            Rect::new(
                area.x + 1,
                area.y,
                area.width.saturating_sub(1),
                area.height,
            ),
        );
        assert!(controls.controls.iter().all(|(rect, _, _)| rect.y == off.y));
        let off_label = controls
            .controls
            .iter()
            .find(|(_, _, action)| *action == ChatAction::Yolo)
            .unwrap()
            .1
            .clone();
        assert!(off_label.contains("YOLO [ ]"));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            off.x,
            off.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            off.x,
            off.y,
        );
        assert!(app.chat.yolo_enabled());
        assert!(app.chat.busy());
        // Move off the button to verify the enabled state remains highlighted.
        app.mouse_col = 0;
        app.mouse_row = 29;
        paint(&mut app, &mut terminal, false);
        assert_eq!(action_area(&app), off, "toggling must not move the control");
        let cell = &terminal.backend().buffer()[(off.x, off.y)];
        assert_eq!(
            cell.fg,
            app.colors.ui.readable_foreground(app.colors.fg_warning, cell.bg, 4.5)
        );
        let area = app.chat.area.unwrap();
        let controls = header_layout(
            &app,
            Rect::new(
                area.x + 1,
                area.y,
                area.width.saturating_sub(1),
                area.height,
            ),
        );
        let on_label = &controls
            .controls
            .iter()
            .find(|(_, _, action)| *action == ChatAction::Yolo)
            .unwrap()
            .1;
        assert!(on_label.contains("YOLO [x]"));
        for (rect, _, _) in &controls.controls {
            assert!(rect.right() <= width);
        }
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            off.x,
            off.y,
        );
        assert!(!app.chat.yolo_enabled());
    }
}

#[test]
fn chat_yolo_hover_explains_once_scope_and_danger_without_eliding_the_warning() {
    for width in [110, 40, 24] {
        let mut app = empty_chat();
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        paint(&mut app, &mut terminal, false);
        let area = app
            .chat
            .hits
            .iter()
            .find(|hit| hit.action == ChatAction::Yolo)
            .unwrap()
            .area;
        app.mouse_col = area.x;
        app.mouse_row = area.y;
        paint(&mut app, &mut terminal, false);
        let contents = text(&terminal);
        for word in [
            "YOLO: Off",
            "Automatically",
            "once",
            "dangerous",
            "overwrite",
            "delete",
        ] {
            assert!(contents.contains(word), "missing {word} at width {width}");
        }
        app.chat_action(ChatAction::Yolo);
        paint(&mut app, &mut terminal, false);
        assert!(text(&terminal).contains("YOLO: On"));
        app.chat_action(ChatAction::Disconnect);
        app.mouse_col = 0;
        app.mouse_row = 29;
        paint(&mut app, &mut terminal, false);
        assert!(
            !app.chat
                .hits
                .iter()
                .any(|hit| hit.action == ChatAction::Yolo)
        );
    }
}

#[test]
fn chat_buttons_use_theme_colors_on_sidebar_and_composer_surfaces() {
    use crate::ui_control_visibility_tests::{
        assert_readable, audit_themes, check_region, snapshot,
    };
    let mut failures = Vec::new();
    for theme in audit_themes() {
        let mut app = super::tests::seeded();
        app.colors = theme.to_theme_colors();
        for yolo in [false, true] {
            if yolo {
                app.chat_action(ChatAction::Yolo);
            }
            assert_eq!(app.chat.yolo_enabled(), yolo);
            for hovered in [false, true] {
                let mut terminal = Terminal::new(TestBackend::new(140, 3)).unwrap();
                let mut areas = Vec::new();
                terminal
                    .draw(|frame| {
                        for (row, surface) in [app.colors.bg_sidebar, app.colors.bg_input]
                            .into_iter()
                            .enumerate()
                        {
                            frame.render_widget(
                                Block::default().style(
                                    Style::default().fg(app.colors.ui.fg_sidebar).bg(surface),
                                ),
                                Rect::new(0, row as u16, 140, 1),
                            );
                            for (index, (label, action, kind)) in [
                                (
                                    "Tool =",
                                    ChatAction::OutputDetail,
                                    widgets::ButtonKind::Toolbar,
                                ),
                                (
                                    "Thinking >",
                                    ChatAction::ThinkingDetail,
                                    widgets::ButtonKind::Toolbar,
                                ),
                                (
                                    "Model / Medium",
                                    ChatAction::Picker(Picker::Models),
                                    widgets::ButtonKind::Dropdown,
                                ),
                                ("Send ↑", ChatAction::Send, widgets::ButtonKind::Primary),
                                ("■ Stop", ChatAction::Stop, widgets::ButtonKind::Secondary),
                                ("YOLO", ChatAction::Yolo, widgets::ButtonKind::Toolbar),
                                ("+ Files", ChatAction::Attach, widgets::ButtonKind::Quiet),
                            ]
                            .into_iter()
                            .enumerate()
                            {
                                let area = Rect::new(index as u16 * 20, row as u16, 19, 1);
                                app.mouse_col = if hovered { area.x + 1 } else { u16::MAX };
                                app.mouse_row = if hovered { area.y } else { u16::MAX };
                                widgets::draw_button(
                                    frame, &mut app, area, label, action, true, kind,
                                );
                                areas.push(area);
                            }
                        }
                    })
                    .unwrap();
                for area in areas {
                    check_region(
                        terminal.backend().buffer(),
                        area,
                        &format!("{}: chat button yolo={yolo}, hover={hovered}", theme.name),
                        &mut failures,
                        &app.colors.ui,
                    );
                }
                if !hovered && !yolo {
                    snapshot(
                        terminal.backend().buffer(),
                        &format!("{} - Chat controls", theme.name),
                    );
                }
            }
        }
    }
    assert_readable(failures);
}
