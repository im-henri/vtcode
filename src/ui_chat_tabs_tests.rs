use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use vtcode::acp::{Event, Operation};

fn open(app: &mut App, name: &str) -> p::SessionId {
    let id = p::SessionId::new(name.to_owned());
    app.apply_acp_event(Event::SessionOpened {
        session_id: id.clone(),
        modes: None,
        config_options: vec![],
        loaded: false,
    });
    app.chat.session_mut().unwrap().title = name.into();
    id
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.text_input_regions.clear();
    terminal.draw(|f| draw(f, app, f.area())).unwrap();
}

fn click(app: &mut App, terminal: &mut Terminal<TestBackend>, action: ChatAction) {
    render(app, terminal);
    let area = app
        .chat
        .hits
        .iter()
        .find(|hit| hit.action == action)
        .unwrap_or_else(|| panic!("missing control: {action:?}"))
        .area;
    assert!(app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::empty(),
    }));
}

fn update(app: &mut App, id: &p::SessionId, value: serde_json::Value) {
    app.apply_acp_event(Event::Update(p::SessionNotification::new(
        id.clone(),
        serde_json::from_value(value).unwrap(),
    )));
}

fn capture(terminal: &Terminal<TestBackend>, name: &str) {
    let Some(directory) = std::env::var_os("VTCODE_CHAT_CAPTURE") else {
        return;
    };
    let buffer = terminal.backend().buffer();
    let cells: Vec<_> = buffer.content.iter().map(|c| serde_json::json!({
        "text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg),"bold":c.modifier.contains(Modifier::BOLD)
    })).collect();
    std::fs::write(std::path::Path::new(&directory).join(format!("chat-tabs-{name}.json")),
        serde_json::to_vec(&serde_json::json!({"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
}

#[test]
fn acp_chat_tabs_keep_drafts_attachments_scroll_and_terminal_input_separate() {
    let mut app = tests::seeded();
    let first = app.chat.selected.clone().unwrap();
    app.paste_into_focused_text_input("First draft");
    app.chat.files.attached.push("first.rs".into());
    app.chat.follow = false;
    app.chat.scroll = 5;
    app.chat.expanded.insert(2);
    let second = open(&mut app, "Second chat");
    assert!(app.chat.input.is_empty());
    assert!(app.chat.files.attached.is_empty());
    assert!(app.chat.follow);
    assert!(app.chat.expanded.is_empty());
    app.paste_into_focused_text_input("Second draft");
    app.chat.files.attached.push("second.rs".into());
    let mut terminal = Terminal::new(TestBackend::new(90, 35)).unwrap();
    click(&mut app, &mut terminal, ChatAction::Switch(first.clone()));
    assert_eq!(app.chat.input, "First draft");
    assert_eq!(
        app.chat.files.attached,
        vec![std::path::PathBuf::from("first.rs")]
    );
    assert_eq!(app.chat.scroll, 5);
    assert!(!app.chat.follow);
    assert!(app.chat.expanded.contains(&2));
    app.handle_chat_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
    assert_eq!(app.chat.input, "");
    app.paste_into_focused_text_input("Saved conversation draft");
    app.chat
        .session_mut()
        .unwrap()
        .terminal(vtcode::acp::TerminalSnapshot {
            session_id: first.clone(),
            id: "shell".into(),
            command: "test".into(),
            output: "".into(),
            truncated: false,
            exit_status: None,
            released: false,
            cwd: Default::default(), history: None, history_version: 0, killed: false, error: None,
        });
    app.chat_action(ChatAction::Terminal("shell".into()));
    app.paste_into_focused_text_input("terminal command");
    click(&mut app, &mut terminal, ChatAction::Switch(second.clone()));
    assert_eq!(app.chat.input, "Second draft");
    assert!(app.chat.terminal_input.is_none());
    click(&mut app, &mut terminal, ChatAction::Switch(first.clone()));
    assert_eq!(app.chat.input, "terminal command");
    assert_eq!(app.chat.terminal_input.as_deref(), Some("shell"));
    app.chat_action(ChatAction::BackToChat);
    assert_eq!(app.chat.input, "Saved conversation draft");
    app.chat_action(ChatAction::Terminal("shell".into()));
    app.chat_action(ChatAction::Switch(second));
    app.apply_acp_event(Event::Terminal(vtcode::acp::TerminalSnapshot {
        session_id: first.clone(),
        id: "shell".into(),
        command: "test".into(),
        output: "done".into(),
        truncated: false,
        exit_status: Some(p::TerminalExitStatus::new().exit_code(0)),
        released: true,
        cwd: Default::default(), history: None, history_version: 0, killed: false, error: None,
    }));
    app.chat_action(ChatAction::Switch(first));
    assert!(app.chat.terminal_input.is_none());
    assert_eq!(app.chat.input, "Saved conversation draft");
}

#[test]
fn acp_chat_tabs_close_reopen_and_route_background_activity_without_stealing_focus() {
    let mut app = tests::seeded();
    let first = app.chat.selected.clone().unwrap();
    let second = open(&mut app, "Review tests");
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Review tests".into());
    app.chat
        .session_mut()
        .unwrap()
        .finish(Some(p::StopReason::EndTurn));
    app.paste_into_focused_text_input("Keep this draft");
    app.chat
        .sessions
        .get_mut(&first)
        .unwrap()
        .begin_prompt("Background task".into());
    update(
        &mut app,
        &first,
        serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Background answer"}}),
    );
    assert_eq!(app.chat.selected.as_ref(), Some(&second));
    assert!(app.chat.unread_tabs.contains(&first));
    assert_eq!(app.chat.input, "Keep this draft");
    app.apply_acp_event(Event::Error {
        operation: Operation::Prompt,
        session_id: Some(first.clone()),
        error: p::Error::new(-32000, "Background failure"),
    });
    assert!(app.chat.error.is_none());
    app.chat_action(ChatAction::Switch(first.clone()));
    assert!(
        app.chat
            .error
            .as_deref()
            .unwrap()
            .contains("Background failure")
    );
    assert!(!app.chat.unread_tabs.contains(&first));
    app.chat_action(ChatAction::Switch(second.clone()));
    assert!(app.chat.error.is_none());
    let mut terminal = Terminal::new(TestBackend::new(90, 35)).unwrap();
    click(&mut app, &mut terminal, ChatAction::CloseTab(first.clone()));
    assert_eq!(app.chat.selected.as_ref(), Some(&second));
    assert!(app.chat.sessions.contains_key(&first));
    click(
        &mut app,
        &mut terminal,
        ChatAction::CloseTab(second.clone()),
    );
    assert!(app.chat.selected.is_none());
    assert!(app.chat.open_tabs.is_empty());
    assert!(app.chat.input.is_empty());
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    let choice = app
        .chat
        .choices
        .iter()
        .position(|c| c.action == ChatAction::Switch(second.clone()))
        .unwrap();
    app.chat_action(ChatAction::Pick(choice));
    assert_eq!(app.chat.open_tabs, vec![second.clone()]);
    assert_eq!(app.chat.input, "Keep this draft");
    app.chat_action(ChatAction::Switch(second.clone()));
    assert_eq!(
        app.chat.open_tabs.len(),
        1,
        "opening history must reuse an existing tab"
    );
    app.chat_action(ChatAction::Switch(first.clone()));
    app.apply_acp_event(Event::SessionClosed(second.clone()));
    assert_eq!(app.chat.selected.as_ref(), Some(&first));
    assert!(!app.chat.sessions.contains_key(&second));
    app.apply_acp_event(Event::SessionClosed(first));
    assert!(app.chat.selected.is_none());
    assert!(app.chat.open_tabs.is_empty());
    app.apply_acp_event(Event::Error {
        operation: Operation::LoadSession,
        session_id: Some(p::SessionId::new("unavailable-history")),
        error: p::Error::new(-32000, "History unavailable"),
    });
    assert!(
        app.chat
            .error
            .as_deref()
            .unwrap()
            .contains("History unavailable")
    );
}

#[test]
fn acp_chat_tabs_scroll_reveal_active_and_follow_tab_theme_colors() {
    for theme in ["dark", "light", "high-contrast", "monokai"] {
        for width in [14, 26, 40, 58, 100] {
            let mut app = tests::seeded();
            app.set_theme_by_key(theme);
            app.chat.session_mut().unwrap().title = "Greeting".into();
            let first = app.chat.selected.clone().unwrap();
            let second = open(&mut app, "Plan refactor");
            let last = open(&mut app, "Review tests");
            app.chat.sessions.get_mut(&second).unwrap().busy = true;
            app.chat_action(ChatAction::Switch(first.clone()));
            update(
                &mut app,
                &last,
                serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Tests look good."}}),
            );
            let mut terminal = Terminal::new(TestBackend::new(width, 35)).unwrap();
            render(&mut app, &mut terminal);
            let tab = app
                .chat
                .hits
                .iter()
                .find(|h| h.action == ChatAction::Switch(first.clone()))
                .unwrap()
                .area;
            assert!((0..width).all(|x| terminal.backend().buffer()[(x, tab.y)].symbol() != "+"),
                "disconnected conversations remain visible without a new-conversation button at width {width}");
            let history = app.chat.hits.iter().find(|hit| hit.action == ChatAction::Picker(Picker::Sessions)).unwrap().area;
            assert!(history.right() <= width, "history remains reachable in narrow panes");
            let cell = terminal
                .backend()
                .buffer()
                .cell((tab.x + 3, tab.y))
                .unwrap();
            assert_eq!(cell.bg, app.colors.bg_tab_active);
            assert_eq!(
                cell.fg,
                app.colors.ui.readable_foreground(app.colors.ui.fg_tab_active, cell.bg, 4.5)
            );
            app.mouse_col = tab.x;
            app.mouse_row = tab.y;
            render(&mut app, &mut terminal);
            assert_eq!(
                terminal.backend().buffer().cell((tab.x, tab.y)).unwrap().bg,
                app.colors.ui.bg_tab_hover
            );
            app.mouse_col = 0;
            app.mouse_row = 0;
            app.chat_action(ChatAction::Switch(last.clone()));
            render(&mut app, &mut terminal);
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::CloseTab(last.clone()))
            );
            for hit in &app.chat.hits {
                assert!(
                    hit.area.right() <= width && hit.area.bottom() <= 35,
                    "{theme} {width}: {:?}",
                    hit.action
                );
            }
            if app.chat.tab_offset > 0 {
                let offset = app.chat.tab_offset;
                let area = app.chat.tab_area.unwrap();
                app.handle_chat_mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    column: area.x + 1,
                    row: area.y,
                    modifiers: KeyModifiers::empty(),
                });
                assert_eq!(app.chat.tab_offset, offset - 1);
            }
            if width == 100 {
                terminal.resize(Rect::new(0, 0, 26, 35)).unwrap();
                render(&mut app, &mut terminal);
                assert!(
                    app.chat
                        .hits
                        .iter()
                        .any(|h| h.action == ChatAction::Switch(last.clone())),
                    "resizing must reveal the selected tab"
                );
                terminal.resize(Rect::new(0, 0, width, 35)).unwrap();
            }
            app.handle_chat_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
            assert_eq!(app.chat.selected.as_ref(), Some(&first));
            app.handle_chat_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL));
            assert_eq!(app.chat.selected.as_ref(), Some(&last));
            app.handle_chat_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
            assert_eq!(app.chat.selected.as_ref(), Some(&second));
            assert_eq!(app.chat.open_tabs.len(), 2);
            if width >= 40 {
                app.chat_action(ChatAction::Switch(first));
                render(&mut app, &mut terminal);
                capture(&terminal, &format!("{theme}-{width}"));
            }
        }
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_multiple_tabs_stream_and_cancel_independently() {
    fn until(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done(app) {
            assert!(
                Instant::now() < deadline,
                "agent timed out: {:?}",
                app.chat.error
            );
            app.poll_acp();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().to_path_buf();
    let mut config = vtcode::acp::AgentConfig::new(
        "Official SDK scenario agent",
        std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md"),
    );
    config.args = vec![
        "-u".into(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scenario_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    app.settings_effective.acp_agents = Some(vec![config]);
    app.focus_chat();
    app.chat_action(ChatAction::Connect(0));
    until(&mut app, |a| a.chat.selected.is_some());
    let first = app.chat.selected.clone().unwrap();
    app.paste_into_focused_text_input("silent");
    app.chat_action(ChatAction::Send);
    until(&mut app, |a| {
        a.chat
            .diagnostics
            .iter()
            .any(|s| s.contains("Silent prompt received"))
    });
    app.paste_into_focused_text_input("Next task for first chat");
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    render(&mut app, &mut terminal);
    let new = app.chat.hits.iter().find(|hit| hit.action == ChatAction::New).unwrap().area;
    let last_tab_end = app.chat.hits.iter().filter(|hit| matches!(hit.action, ChatAction::Switch(_)))
        .map(|hit| hit.area.right()).max().unwrap();
    assert_eq!(new.x, last_tab_end, "new conversation follows the last visible tab when connected");
    click(&mut app, &mut terminal, ChatAction::New);
    until(&mut app, |a| a.chat.open_tabs.len() == 2);
    let second = app.chat.selected.clone().unwrap();
    assert!(app.chat.input.is_empty());
    assert!(app.chat.sessions[&first].busy);
    app.paste_into_focused_text_input("Hello from the second chat");
    app.chat_action(ChatAction::Send);
    click(&mut app, &mut terminal, ChatAction::Switch(first.clone()));
    assert_eq!(app.chat.input, "Next task for first chat");
    until(&mut app, |a| !a.chat.sessions[&second].busy);
    assert!(app.chat.sessions[&first].busy);
    assert!(app.chat.unread_tabs.contains(&second));
    assert!(app.chat.sessions[&second].entries.iter().any(
        |e| matches!(e, ChatEntry::Agent(text) if text.contains("Hello from the second chat"))
    ));
    // Closing a working tab keeps its request alive and recoverable from History.
    click(&mut app, &mut terminal, ChatAction::CloseTab(first.clone()));
    assert!(app.chat.sessions[&first].busy);
    assert_eq!(app.chat.selected.as_ref(), Some(&second));
    app.paste_into_focused_text_input("permission");
    app.chat_action(ChatAction::Send);
    app.chat_action(ChatAction::Switch(first.clone()));
    until(&mut app, |a| {
        a.chat
            .permissions
            .iter()
            .any(|p| p.request.session_id == second)
    });
    assert!(app.chat.permission().is_none());
    render(&mut app, &mut terminal);
    assert!(terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect::<String>().contains("Queue up"));
    app.chat.focus = None;
    app.handle_chat_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
    until(&mut app, |a| !a.chat.sessions[&first].busy);
    assert!(
        app.chat.sessions[&second].busy,
        "cancelling one tab must not cancel another"
    );
    click(&mut app, &mut terminal, ChatAction::Switch(second.clone()));
    assert!(app.chat.permission().is_some());
    let permission = app.chat.permission().unwrap();
    let action = ChatAction::Permission(
        permission.id.clone(),
        permission.ticket,
        Some(permission.request.options[1].option_id.clone()),
    );
    click(&mut app, &mut terminal, action);
    until(&mut app, |a| !a.chat.sessions[&second].busy);
    assert_eq!(
        app.chat.sessions[&first].stop_reason,
        Some(p::StopReason::Cancelled)
    );
    assert!(app.chat.error.is_none());
    assert_eq!(app.chat.open_tabs.len(), 2);
}
