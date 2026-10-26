use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use vtcode::acp::{Event, TerminalSnapshot, terminal_history::TerminalHistory};

fn fixture() -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    let history = Arc::new(TerminalHistory::new(directory.path()).unwrap());
    let mut output = "[agent command] cargo test --all\n".to_owned();
    for i in 0..200 {
        output.push_str(&format!(
            "line-{i:03} 世界 {} END\n",
            "long output ".repeat(6)
        ));
    }
    let version = history.append(output.as_bytes()).unwrap();
    let mut app = App::test_new();
    app.chat.visible = true;
    app.active_pane = ActivePane::Chat;
    app.chat.agent = Some(vtcode::acp::AgentConfig::new("Workspace Agent", "test"));
    let id = p::SessionId::new("terminal-chat");
    let mut session = Session::new(id.clone());
    session.push(ChatEntry::User("Run checks".into()));
    session.terminal(TerminalSnapshot {
        session_id: id.clone(),
        id: "test-run".into(),
        command: "cargo test --all".into(),
        cwd: directory.path().into(),
        output: "protocol tail only".into(),
        truncated: true,
        exit_status: None,
        released: false,
        killed: false,
        error: None,
        history: Some(history),
        history_version: version,
    });
    app.chat.sessions.insert(id.clone(), session);
    app.chat.selected = Some(id.clone());
    app.chat.open_tabs.push(id);
    (directory, app)
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.text_input_regions.clear();
    terminal.draw(|f| draw(f, app, f.area())).unwrap();
}
fn wait(app: &mut App, terminal: &mut Terminal<TestBackend>, ready: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.poll_acp();
        render(app, terminal);
        if ready(app) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "terminal viewport timed out: error={:?}, status={}, busy={}, outputs={:?}",
            app.chat.error, app.chat.status, app.chat.busy(),
            app.chat.session().map(|s| s.terminals.values().map(|t| (&t.id, &t.output, &t.exit_status)).collect::<Vec<_>>())
        );
        std::thread::sleep(Duration::from_millis(3));
    }
}
fn click(app: &mut App, terminal: &mut Terminal<TestBackend>, action: ChatAction) {
    render(app, terminal);
    let rect = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == action)
        .unwrap_or_else(|| panic!("missing terminal control {action:?}"))
        .area;
    assert!(app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::empty()
    }));
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

#[test]
fn agent_terminal_minimal_header_keeps_controls_and_expands_to_details() {
    let (_directory, mut app) = fixture();
    app.chat.output_detail = crate::app_acp::OutputDetail::Minimal;
    for width in [8, 24, 36, 80] {
        let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
        render(&mut app, &mut terminal);
        let screen = text(&terminal);
        assert!(!screen.contains("Read-only"));
        assert!(!screen.contains("protocol tail only"));
        assert!(app.chat.terminal_views.is_empty());
        assert!(app.chat.hits.iter().all(|h| h.area.right() <= width));
        if width >= 24 {
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::StopTerminal("test-run".into()))
            );
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::ToggleEntry(1))
            );
        }
        if width == 80 {
            assert_eq!(app.chat.cache.entry_height(1), 3);
            assert!(screen.contains("Running"));
            assert!(screen.contains("cargo test --all"));
            let toggle = app.chat.hits.iter().find(|h| h.action == ChatAction::ToggleEntry(1)).unwrap().area;
            click(&mut app, &mut terminal, ChatAction::ToggleEntry(1));
            render(&mut app, &mut terminal);
            assert_eq!(toggle, app.chat.hits.iter().find(|h| h.action == ChatAction::ToggleEntry(1)).unwrap().area);
            assert!(text(&terminal).contains("Collapse"));
            assert!(text(&terminal).contains("Read-only"));
            assert!(app.chat.terminal_views.contains_key("test-run"));
        }
    }
}

#[test]
fn agent_terminal_inline_viewport_scrolls_independently_and_stays_read_only() {
    let (_directory, mut app) = fixture();
    let mut terminal = Terminal::new(TestBackend::new(72, 40)).unwrap();
    wait(&mut app, &mut terminal, |a| {
        a.chat
            .terminal_views
            .get("test-run")
            .is_some_and(|v| v.page.is_some())
    });
    assert!(text(&terminal).contains("Agent terminal"));
    assert!(text(&terminal).contains("Read-only"));
    assert!(app.chat.terminal_input.is_none());
    click(&mut app, &mut terminal, ChatAction::ToggleEntry(1));
    wait(&mut app, &mut terminal, |a| {
        a.chat.terminal_views["test-run"]
            .page
            .as_ref()
            .unwrap()
            .lines
            .len()
            == 10
    });
    assert_eq!(app.chat.terminal_areas[0].1.height, 10);
    let chat_scroll = app.chat.scroll;
    let first = app.chat.terminal_views["test-run"]
        .page
        .as_ref()
        .unwrap()
        .first;
    let area = app.chat.terminal_areas[0].1;
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: area.x + 1,
        row: area.y + 1,
        modifiers: KeyModifiers::empty(),
    });
    wait(&mut app, &mut terminal, |a| {
        a.chat.terminal_views["test-run"]
            .page
            .as_ref()
            .unwrap()
            .first
            == first - 3
    });
    assert_eq!(app.chat.scroll, chat_scroll);
    app.handle_chat_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
    assert!(app.chat.input.is_empty());
    assert!(app.chat.terminal_input.is_none());
    app.chat.focus = Some(ChatAction::FocusTerminal("test-run".into()));
    app.handle_chat_key(KeyEvent::new(KeyCode::Home, KeyModifiers::empty()));
    wait(&mut app, &mut terminal, |a| {
        a.chat.terminal_views["test-run"]
            .page
            .as_ref()
            .unwrap()
            .first
            == 0
    });
    assert!(text(&terminal).contains("[agent command] cargo test"));
    assert!(text(&terminal).contains("line-000"));
    for _ in 0..3 { app.handle_chat_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty())); }
    assert_eq!(app.chat.terminal_views["test-run"].column, 12);
    app.chat_action(ChatAction::TerminalTop("test-run".into()));
    let mut snapshot = app.chat.session().unwrap().terminals["test-run"].clone();
    snapshot.history_version = snapshot
        .history
        .as_ref()
        .unwrap()
        .append(b"later output\n")
        .unwrap();
    app.apply_acp_event(Event::Terminal(snapshot));
    wait(&mut app, &mut terminal, |a| {
        a.chat.terminal_views["test-run"]
            .page
            .as_ref()
            .unwrap()
            .total
            == 202
    });
    assert_eq!(
        app.chat.terminal_views["test-run"]
            .page
            .as_ref()
            .unwrap()
            .first,
        0
    );
    app.chat.focus = Some(ChatAction::FocusTerminal("test-run".into()));
    app.handle_chat_key(KeyEvent::new(KeyCode::End, KeyModifiers::empty()));
    wait(&mut app, &mut terminal, |a| {
        a.chat.terminal_views["test-run"]
            .page
            .as_ref()
            .unwrap()
            .lines
            .last()
            .is_some_and(|l| l == "later output")
    });
    app.chat.input = "my chat draft".into();
    click(
        &mut app,
        &mut terminal,
        ChatAction::Terminal("test-run".into()),
    );
    assert_eq!(app.chat.terminal_input.as_deref(), Some("test-run"));
    assert!(app.chat.input.is_empty());
    click(&mut app, &mut terminal, ChatAction::BackToChat);
    assert_eq!(app.chat.input, "my chat draft");
}

#[test]
fn agent_terminal_cards_keep_kill_visible_in_narrow_themed_layouts() {
    let (_directory, mut app) = fixture();
    for theme in ["dark", "light", "high-contrast"] {
        app.set_theme_by_key(theme);
        for width in [36, 52, 100] {
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            app.chat_action(ChatAction::RevealTerminal("test-run".into()));
            wait(&mut app, &mut terminal, |a| {
                a.chat.terminal_views.get("test-run").is_some_and(|v| {
                    v.width == width as usize - 7
                        && v.page.as_ref().is_some_and(|p| {
                            p.lines.first()
                                == Some(&vtcode::acp::terminal_history::window(
                                    &format!("line-190 世界 {} END", "long output ".repeat(6)),
                                    0,
                                    width as usize - 7,
                                ))
                        })
                })
            });
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::StopTerminal("test-run".into()))
            );
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::Terminal("test-run".into()))
            );
            assert!(!app.chat.hits.iter().any(|h| matches!(h.action, ChatAction::PanTerminal(..) | ChatAction::ScrollTerminal(..) | ChatAction::TerminalTop(..) | ChatAction::FollowTerminal(..))));
            if let Some(directory) = std::env::var_os("VTCODE_TERMINAL_CAPTURE") {
                let directory = std::path::PathBuf::from(directory);
                std::fs::create_dir_all(&directory).unwrap();
                let buffer = terminal.backend().buffer();
                let cells: Vec<_> = buffer.content.iter().map(|c| serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)})).collect();
                std::fs::write(
                    directory.join(format!("terminal-{theme}-{width}.json")),
                    serde_json::to_vec(
                        &serde_json::json!({"width":width,"height":40,"cells":cells}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
        }
    }
}

#[test]
fn agent_terminal_views_remain_scoped_to_their_chat_and_tool_links_do_not_unlock_input() {
    let (_directory, mut app) = fixture();
    let mut terminal = Terminal::new(TestBackend::new(72, 40)).unwrap();
    app.chat_action(ChatAction::RevealTerminal("test-run".into()));
    wait(&mut app, &mut terminal, |a| {
        a.chat
            .terminal_views
            .get("test-run")
            .is_some_and(|v| v.page.is_some())
    });
    assert!(app.chat.terminal_input.is_none());
    app.chat_action(ChatAction::TerminalTop("test-run".into()));
    let first = app.chat.selected.clone().unwrap();
    let second = p::SessionId::new("another-chat");
    app.apply_acp_event(Event::SessionOpened {
        session_id: second.clone(),
        modes: None,
        config_options: vec![],
        loaded: false,
    });
    assert!(app.chat.terminal_views.is_empty());
    app.chat_action(ChatAction::Switch(first));
    assert_eq!(app.chat.terminal_views["test-run"].first, Some(0));
    assert!(app.chat.terminal_input.is_none());
}

#[test]
#[ignore = "requires official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_agent_terminal_input_history_and_scoped_kill() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer.root = directory.path().into();
    app.chat.visible = true;
    app.active_pane = ActivePane::Chat;
    let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
    let mut config = vtcode::acp::AgentConfig::new("Terminal SDK", python);
    config.args = vec![
        "-u".into(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scenario_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    app.settings_effective.acp_agents = Some(vec![config]);
    app.chat_action(ChatAction::Connect(0));
    let mut terminal = Terminal::new(TestBackend::new(100, 44)).unwrap();
    wait(&mut app, &mut terminal, |a| {
        a.chat.selected.is_some() && !a.chat.rollback.busy()
    });
    app.chat.input = "terminal-history".into();
    app.chat_action(ChatAction::Send);
    wait(&mut app, &mut terminal, |a| {
        a.chat
            .session()
            .unwrap()
            .terminals
            .values()
            .any(|t| t.output.contains("ready"))
    });
    let id = app
        .chat
        .session()
        .unwrap()
        .terminals
        .keys()
        .next()
        .unwrap()
        .clone();
    app.chat_action(ChatAction::RevealTerminal(id.clone()));
    wait(&mut app, &mut terminal, |a| {
        a.chat
            .terminal_views
            .get(&id)
            .is_some_and(|v| v.page.is_some())
    });
    app.chat.focus = Some(ChatAction::FocusTerminal(id.clone()));
    app.handle_chat_key(KeyEvent::new(KeyCode::Home, KeyModifiers::empty()));
    wait(&mut app, &mut terminal, |a| {
        // The compact preview may still hold the initial command-only page.
        // Wait for the expanded page to load the process output as well.
        a.chat.terminal_views[&id].page.as_ref().is_some_and(|page| {
            page.first == 0 && page.lines.iter().any(|line| line.contains("line-000"))
        })
    });
    assert!(text(&terminal).contains("line-000"));
    assert!(app.chat.session().unwrap().terminals[&id].truncated);
    click(&mut app, &mut terminal, ChatAction::Terminal(id.clone()));
    app.chat.input = "input from user 世界".into();
    app.chat_action(ChatAction::Send);
    wait(&mut app, &mut terminal, |a| !a.chat.busy());
    assert!(app.chat.terminal_input.is_none());
    let saved = &app.chat.session().unwrap().terminals[&id];
    let tail = saved
        .history
        .as_ref()
        .unwrap()
        .page(None, 0, 120, 10)
        .unwrap();
    assert!(
        tail.lines
            .iter()
            .any(|l| l == "[user input] input from user 世界")
    );
    assert!(
        tail.lines
            .iter()
            .any(|l| l == "received=input from user 世界")
    );
    app.chat.input = "terminal-two".into();
    app.chat_action(ChatAction::Send);
    wait(&mut app, &mut terminal, |a| {
        a.chat
            .session()
            .unwrap()
            .terminals
            .values()
            .filter(|t| !t.released && t.output.contains("ready"))
            .count()
            == 2
    });
    let mut ids: Vec<_> = app
        .chat
        .session()
        .unwrap()
        .terminals
        .values()
        .filter(|t| !t.released)
        .map(|t| t.id.clone())
        .collect();
    ids.sort();
    app.chat_action(ChatAction::RevealTerminal(ids[0].clone()));
    click(
        &mut app,
        &mut terminal,
        ChatAction::StopTerminal(ids[0].clone()),
    );
    wait(&mut app, &mut terminal, |a| {
        a.chat.session().unwrap().terminals[&ids[0]]
            .exit_status
            .is_some()
    });
    assert!(
        app.chat.session().unwrap().terminals[&ids[1]]
            .exit_status
            .is_none()
    );
    app.chat_action(ChatAction::RevealTerminal(ids[1].clone()));
    click(
        &mut app,
        &mut terminal,
        ChatAction::StopTerminal(ids[1].clone()),
    );
    wait(&mut app, &mut terminal, |a| !a.chat.busy());
    assert!(
        ids.iter()
            .all(|id| app.chat.session().unwrap().terminals[id].killed)
    );
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    app.chat_action(ChatAction::Disconnect);
}
