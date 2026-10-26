use super::*;
use crate::app::{Tab, TabDropZone};
use ratatui::{Terminal, backend::TestBackend};
use std::time::{Duration, Instant};
#[path = "app_acp_agent_close_tests.rs"]
mod close_tests;

fn state(name: &str) -> AgentState {
    let id = p::SessionId::new("1");
    let mut session = Session::new(id.clone());
    session.begin_prompt(format!("{name} request"));
    session.push(ChatEntry::Agent(format!("{name} reply")));
    session.finish(Some(p::StopReason::EndTurn));
    session.title = format!("{name} conversation");
    AgentState {
        agent: Some(AgentConfig::new(name, format!("{name}-agent"))),
        sessions: [(id.clone(), session)].into(),
        selected: Some(id.clone()),
        dock_selected: Some(id.clone()),
        session_order: vec![id.clone()],
        open_tabs: vec![id],
        input: format!("{name} draft"),
        ..Default::default()
    }
}

fn app() -> App {
    let mut app = App::test_new();
    app.chat.connection = state("OpenCode");
    app.chat.connections.insert(1, state("Copilot"));
    app.chat.agent_order = vec![0, 1];
    app.chat.next_agent = 2;
    app.chat.visible = true;
    app.chat.width = 65;
    app.active_pane = ActivePane::Chat;
    app.show_explorer = false;
    app.show_panel = false;
    app.terminal_sidebar_revealed = false;
    app
}

#[test]
fn exhausted_shared_budget_does_not_reschedule_idle_agents_forever() {
    let mut app = app();
    let generation = app.background.wake_handle().generation();
    let expired = Instant::now();
    for _ in 0..4 {
        app.poll_acp_until(expired);
        assert_eq!(app.chat.next_poll_agent, 0);
        assert_eq!(app.chat.active_agent, 0);
        assert_eq!(app.chat.input, "OpenCode draft");
        assert_eq!(app.chat.agent_state(1).unwrap().input, "Copilot draft");
    }
    assert_eq!(app.background.wake_handle().generation(), generation);
    app.chat.agent_order.remove(1);
    app.chat.connections.remove(&1);
    app.poll_acp_until(expired);
    assert_eq!(app.chat.next_poll_agent, 0, "removing an agent keeps rotation valid");
}

fn paint(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.text_input_regions.clear();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
}

fn click(app: &mut App, action: ChatAction) {
    let hit = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == action)
        .unwrap_or_else(|| panic!("Missing {action:?}"))
        .area;
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: hit.x,
            row: hit.y,
            modifiers: KeyModifiers::empty(),
        },
    );
}

fn text(terminal: &Terminal<TestBackend>) -> String {
    let b = terminal.backend().buffer();
    (0..b.area.height)
        .map(|y| {
            (0..b.area.width)
                .map(|x| b[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn capture(terminal: &Terminal<TestBackend>, name: &str) {
    let Some(dir) = std::env::var_os("VTCODE_CHAT_PRESENTATION_CAPTURE") else {
        return;
    };
    let b = terminal.backend().buffer();
    let cells: Vec<_> = b
        .content
        .iter()
        .map(|c| {
            serde_json::json!({
                "text": c.symbol(), "fg": format!("{:?}", c.fg), "bg": format!("{:?}", c.bg),
                "bold": c.modifier.contains(ratatui::style::Modifier::BOLD),
            })
        })
        .collect();
    std::fs::write(
        PathBuf::from(dir).join(format!("agents-{name}.json")),
        serde_json::to_vec(
            &serde_json::json!({"width": b.area.width, "height": b.area.height, "cells": cells}),
        )
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn agent_tabs_keep_conversations_drafts_and_activity_separate() {
    let mut app = app();
    let first = app.chat.selected.clone().unwrap();
    app.chat.files.attached.push("opencode.rs".into());
    app.chat.expanded.insert(1);
    app.chat.follow = false;
    app.chat.scroll = 7;
    app.chat.session_mut().unwrap().busy = true;
    app.chat
        .queues
        .entry(first.clone())
        .or_default()
        .items
        .push_back(queue::QueuedPrompt {
            id: 1,
            text: "OpenCode queued".into(),
            files: vec![],
            paste: None,
        });
    app.select_chat_agent(1);
    assert_eq!(app.chat.input, "Copilot draft");
    assert!(app.chat.files.attached.is_empty());
    assert!(app.chat.expanded.is_empty());
    assert!(!app.chat.busy());
    assert!(app.chat.any_agent_running());
    let other = p::SessionId::new("2");
    app.apply_acp_event(Event::SessionOpened {
        session_id: other.clone(),
        modes: None,
        config_options: vec![],
        loaded: false,
    });
    app.chat.input = "Second Copilot draft".into();
    app.with_agent_context(0, true, |app| {
        app.apply_acp_event(Event::Update(p::SessionNotification::new(first.clone(),
            serde_json::from_value(serde_json::json!({"sessionUpdate":"agent_message_chunk", "content":{"type":"text","text":" Background OpenCode stream"}})).unwrap())));
    });
    assert_eq!(app.chat.active_agent, 1);
    assert_eq!(app.chat.selected.as_ref(), Some(&other));
    assert!(
        app.chat
            .agent_state(0)
            .unwrap()
            .unread_tabs
            .contains(&first)
    );
    let config = app.chat.agent_state(0).unwrap().agent.clone().unwrap();
    app.connect_acp(config); // Reuses the existing connection, even with queued work.
    assert_eq!(app.chat.active_agent, 0);
    assert_eq!(app.chat.input, "OpenCode draft");
    assert_eq!(app.chat.files.attached, vec![PathBuf::from("opencode.rs")]);
    assert!(app.chat.expanded.contains(&1));
    assert_eq!(app.chat.scroll, 7);
    assert!(!app.chat.follow);
    assert!(app.chat.busy());
    assert_eq!(app.chat.queues[&first].items[0].text, "OpenCode queued");
    assert_eq!(app.chat.agent_order, vec![0, 1]);
    app.select_chat_agent(1);
    assert_eq!(app.chat.selected.as_ref(), Some(&other));
    assert_eq!(app.chat.input, "Second Copilot draft");
    app.chat_action(ChatAction::Switch(first));
    assert_eq!(app.chat.input, "Copilot draft");
}

#[test]
fn agent_tabs_mark_background_completion_for_the_right_agent_with_colliding_session_ids() {
    let mut app = app();
    let id = app.chat.selected.clone().unwrap();
    app.chat.session_mut().unwrap().begin_prompt("OpenCode work".into());
    app.select_chat_agent(1);
    let mut terminal = Terminal::new(TestBackend::new(150, 35)).unwrap();
    paint(&mut app, &mut terminal);
    let icon = |app: &App, terminal: &Terminal<TestBackend>, action| {
        let hit = app.chat.hits.iter().find(|h| h.action == action).unwrap();
        terminal.backend().buffer()[(hit.area.x + 1, hit.area.y)].symbol().to_owned()
    };
    assert_eq!(icon(&app, &terminal, ChatAction::SwitchAgent(0)), "⠋");
    assert_eq!(icon(&app, &terminal, ChatAction::Switch(id.clone())), "◇");
    assert!(crate::ui_chat::next_activity_frame(&app).is_some());
    app.with_agent_context(0, true, |app| {
        app.apply_acp_event(Event::PromptFinished {
            session_id: id.clone(),
            response: serde_json::from_value(serde_json::json!({"stopReason":"end_turn"})).unwrap(),
        });
    });
    paint(&mut app, &mut terminal);
    assert_eq!(icon(&app, &terminal, ChatAction::SwitchAgent(0)), "●");
    assert_eq!(icon(&app, &terminal, ChatAction::Switch(id.clone())), "◇");
    assert_eq!(app.chat.active_agent, 1);
    assert!(app.chat.agent_state(0).unwrap().unread_tabs.contains(&id));
    assert!(!app.chat.agent_state(1).unwrap().unread_tabs.contains(&id));
    assert_eq!(app.chat.input, "Copilot draft");
    assert_eq!(crate::ui_chat::next_activity_frame(&app), None);
    capture(&terminal, "unread-background-completion");
    app.select_chat_agent(0);
    paint(&mut app, &mut terminal);
    assert_eq!(icon(&app, &terminal, ChatAction::Switch(id.clone())), "◇");
    assert!(!app.chat.unread_tabs.contains(&id));
}

#[test]
fn agent_tabs_render_and_switch_their_conversation_row() {
    for theme in ["dark", "light", "high-contrast"] {
        let mut app = app();
        app.set_theme_by_key(theme);
        let mut terminal = Terminal::new(TestBackend::new(140, 42)).unwrap();
        paint(&mut app, &mut terminal);
        capture(&terminal, theme);
        assert!(
            text(&terminal).contains("OpenCode convers"),
            "{}",
            text(&terminal)
        );
        let agent_y = app
            .chat
            .hits
            .iter()
            .find(|h| h.action == ChatAction::SwitchAgent(1))
            .unwrap()
            .area
            .y;
        let conversation_y = app.chat.tab_area.unwrap().y;
        assert!(agent_y < conversation_y);
        let last_tab_end = app.chat.hits.iter().filter(|hit| matches!(hit.action, ChatAction::SwitchAgent(_)))
            .map(|hit| hit.area.right()).max().unwrap();
        let add_agent = app.chat.hits.iter().find(|hit| hit.action == ChatAction::Picker(Picker::Agents)).unwrap().area;
        assert_eq!(add_agent.x, last_tab_end, "add agent follows the tabs");
        assert!(app.chat.hits.iter().any(|hit| hit.action == ChatAction::Picker(Picker::Sessions)));

        capture(&terminal, theme);
        click(&mut app, ChatAction::SwitchAgent(1));
        app.mouse_col = u16::MAX; // Leave the tab's tooltip before inspecting the row below.
        paint(&mut app, &mut terminal);
        assert_eq!(app.chat.active_agent, 1);
        assert!(text(&terminal).contains("Copilot convers"));
        assert!(!text(&terminal).contains("OpenCode convers"));
        assert_eq!(app.chat.input, "Copilot draft");
        click(&mut app, ChatAction::SwitchAgent(0));
        assert_eq!(app.chat.input, "OpenCode draft");
    }
}

#[test]
fn agent_tab_overflow_keeps_the_picker_and_selected_agent_reachable() {
    let mut app = app();
    for id in 2..8 {
        app.chat
            .connections
            .insert(id, state(&format!("Agent {id}")));
        app.chat.agent_order.push(id);
    }
    let mut terminal = Terminal::new(TestBackend::new(45, 38)).unwrap();
    app.select_chat_agent(7);
    paint(&mut app, &mut terminal);
    assert!(
        app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::SwitchAgent(7))
    );
    assert!(
        app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::Picker(Picker::Agents))
    );
    assert!(app.chat.agent_tab_offset > 0);
    let area = app.chat.agent_tab_area.unwrap();
    let last_tab_end = app.chat.hits.iter().filter(|hit| matches!(hit.action, ChatAction::SwitchAgent(_)))
        .map(|hit| hit.area.right()).max().unwrap();
    let add_agent = app.chat.hits.iter().find(|hit| hit.action == ChatAction::Picker(Picker::Agents)).unwrap().area;
    assert_eq!(add_agent.x, last_tab_end, "overflow keeps add agent beside the visible tabs");
    assert!(app.chat.hits.iter().filter(|hit| matches!(hit.action, ChatAction::AgentPage(_)))
        .all(|hit| !hit.area.intersects(add_agent)));
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::empty(),
        },
    );
    let offset = app.chat.agent_tab_offset;
    paint(&mut app, &mut terminal);
    assert_eq!(
        app.chat.agent_tab_offset, offset,
        "manual scrolling must not jump to the selected agent"
    );
    assert!(offset < 7);
    capture(&terminal, "narrow");
}

#[test]
fn placed_chats_with_identical_session_ids_keep_their_agent_identity() {
    let mut app = app();
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    let id = app.chat.selected.clone().unwrap();
    app.place_chat_in_editor(id.clone(), 0, None);
    let a = app.chat.layout.active.unwrap();
    app.select_chat_agent(1);
    app.place_chat_in_editor(id.clone(), 0, Some(TabDropZone::Right));
    let b = app.chat.layout.active.unwrap();
    assert_ne!(a, b);
    assert_eq!(app.chat.layout.editors[&a].agent, 0);
    assert_eq!(app.chat.layout.editors[&b].agent, 1);
    let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
    paint(&mut app, &mut terminal);
    assert!(text(&terminal).contains("OpenCode reply"));
    assert!(text(&terminal).contains("Copilot reply"));
    let surface = app
        .chat
        .layout
        .surfaces
        .iter()
        .find(|s| s.key == Some(a))
        .unwrap()
        .clone();
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: surface.area.x + 3,
            row: surface.area.bottom() - 5,
            modifiers: KeyModifiers::empty(),
        },
    );
    assert_eq!(app.chat.active_agent, 0);
    assert_eq!(app.chat.input, "OpenCode draft");
    app.chat.input.push_str(" edited");
    app.chat_action(ChatAction::MoveToDock);
    assert_eq!(app.chat.layout.dock_agent, 0);
    assert!(!app.chat.layout.editors.contains_key(&a));
    assert!(app.chat.layout.editors.contains_key(&b));
    paint(&mut app, &mut terminal);
    assert_eq!(app.chat.input, "OpenCode draft edited");
    app.chat.switch_connection(1);
    assert!(app.activate_editor_chat(&id));
    assert_eq!(app.chat.input, "Copilot draft");
    app.chat_action(ChatAction::CloseTab(id.clone()));
    assert!(app.chat.agent_state(0).unwrap().sessions.contains_key(&id));
    assert!(app.chat.agent_state(1).unwrap().sessions.contains_key(&id));
    assert!(app.chat.layout.editors.is_empty());
}

fn click_surface(app: &mut App, key: Option<u64>, action: ChatAction) {
    let area = app
        .chat
        .layout
        .surfaces
        .iter()
        .find(|surface| surface.key == key)
        .and_then(|surface| surface.hits.iter().find(|hit| hit.action == action))
        .unwrap_or_else(|| panic!("Missing {action:?} on surface {key:?}"))
        .area;
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + area.width / 2,
            row: area.y,
            modifiers: KeyModifiers::empty(),
        },
    );
}

fn click_global_chat_button(app: &mut App) {
    let area = crate::ui_chrome::agent_chat_button_area(app.menu_bar_area.unwrap()).unwrap();
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 2,
            row: area.y,
            modifiers: KeyModifiers::empty(),
        },
    );
}

#[test]
fn chat_dock_keeps_other_agents_accessible_after_dragging_the_only_conversation() {
    let mut app = app();
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    let id = app.chat.selected.clone().unwrap();
    app.chat.session_mut().unwrap().busy = true;
    let mut terminal = Terminal::new(TestBackend::new(170, 45)).unwrap();
    paint(&mut app, &mut terminal);
    let editor = app.editor_area.unwrap();
    click_surface(&mut app, None, ChatAction::Switch(id.clone()));
    for kind in [
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column: editor.x + editor.width / 2,
                row: editor.y + editor.height / 2,
                modifiers: KeyModifiers::empty(),
            },
        );
    }
    let key = app.chat.layout.active.unwrap();
    for _ in 0..3 {
        paint(&mut app, &mut terminal);
        assert!(app.chat.layout.surfaces.iter().any(|s| s.key.is_none()));
        assert!(app.chat.layout.surfaces.iter().any(|s| s.key == Some(key)));
    }
    click_surface(&mut app, None, ChatAction::SwitchAgent(1));
    paint(&mut app, &mut terminal);
    assert_eq!(app.chat.active_agent, 1);
    assert_eq!(app.chat.input, "Copilot draft");
    assert!(text(&terminal).contains("OpenCode reply"));
    assert!(text(&terminal).contains("Copilot reply"));
    assert_eq!(app.chat.layout.editors[&key].agent, 0);
    assert!(app.chat.agent_state(0).unwrap().sessions[&id].busy);
    capture(&terminal, "detached-and-docked");
    click_surface(&mut app, None, ChatAction::SwitchAgent(0));
    paint(&mut app, &mut terminal);
    assert!(app.chat.selected.is_none());
    assert!(app.activate_editor_chat(&id));
    assert_eq!(app.chat.input, "OpenCode draft");
}

#[test]
fn chat_dock_closes_and_reopens_independently_of_editor_conversations() {
    for same_agent in [true, false] {
        for split in [None, Some(TabDropZone::Right)] {
            let mut app = app();
            app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
            app.active_tab = 0;
            let id = app.chat.selected.clone().unwrap();
            app.chat.session_mut().unwrap().busy = true;
            let entries = app.chat.session().unwrap().entries.len();
            let (dock_agent, dock_id, draft) = if same_agent {
                // Also exercise the case with just one ACP and two conversations.
                app.chat.connections.clear();
                app.chat.agent_order = vec![0];
                let second = p::SessionId::new("second");
                app.chat
                    .sessions
                    .insert(second.clone(), Session::new(second.clone()));
                app.chat.open_tab(second.clone());
                app.chat.input = "Second draft".into();
                (0, second, "Second draft")
            } else {
                app.select_chat_agent(1);
                app.chat.switch_connection(0);
                (1, id.clone(), "Copilot draft")
            };
            app.place_chat_in_editor(id.clone(), 0, split);
            let key = app.chat.layout.active.unwrap();
            let mut terminal = Terminal::new(TestBackend::new(180, 55)).unwrap();
            paint(&mut app, &mut terminal);
            assert_eq!(app.chat.input, "OpenCode draft");
            click_surface(&mut app, None, ChatAction::Hide);
            for _ in 0..3 {
                paint(&mut app, &mut terminal);
                assert!(app.chat.layout.surfaces.iter().all(|s| s.key.is_some()));
                assert!(app.chat.layout.surfaces.iter().any(|s| s.key == Some(key)));
            }
            for _ in 0..2 {
                click_global_chat_button(&mut app);
                paint(&mut app, &mut terminal);
                assert!(app.chat.layout.surfaces.iter().any(|s| s.key.is_none()));
                assert_eq!(app.chat.active_agent, dock_agent);
                assert_eq!(app.chat.selected.as_ref(), Some(&dock_id));
                assert_eq!(app.chat.input, draft);
                click_global_chat_button(&mut app);
                for _ in 0..3 {
                    paint(&mut app, &mut terminal);
                    assert!(app.chat.layout.surfaces.iter().all(|s| s.key.is_some()));
                }
            }
            assert_eq!(app.chat.layout.editors[&key].agent, 0);
            app.chat.switch_connection(0);
            assert!(app.activate_editor_chat(&id));
            assert_eq!(app.chat.input, "OpenCode draft");
            assert_eq!(app.chat.session().unwrap().entries.len(), entries);
            assert!(app.chat.session().unwrap().busy);
            // Closing an embedded view must not close the independent dock.
            click_global_chat_button(&mut app);
            paint(&mut app, &mut terminal);
            click_surface(&mut app, Some(key), ChatAction::Hide);
            paint(&mut app, &mut terminal);
            assert!(app.chat.layout.surfaces.iter().any(|s| s.key.is_none()));
            assert!(!app.chat.layout.editors.contains_key(&key));
            assert!(app.chat.agent_state(0).unwrap().sessions[&id].busy);
        }
    }
}

#[test]
fn background_agent_opening_a_conversation_does_not_steal_a_pane() {
    let mut app = app();
    app.chat.switch_connection(1);
    app.chat.opening_group = Some(0);
    app.chat.switch_connection(0);
    let pane = app.active_pane;
    app.with_agent_context(1, true, |app| {
        app.apply_acp_event(Event::SessionOpened {
            session_id: p::SessionId::new("background"),
            modes: None,
            config_options: vec![],
            loaded: false,
        })
    });
    assert_eq!(app.chat.active_agent, 0);
    assert_eq!(app.chat.layout.dock_agent, 0);
    assert_eq!(app.active_pane, pane);
    assert!(app.chat.layout.active.is_none());
    assert!(app.chat.layout.editors.values().any(|view| view.agent == 1));
    assert_eq!(app.chat.input, "OpenCode draft");
}

#[test]
fn background_file_search_results_are_scoped_to_their_agent() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("alpha.rs"), "alpha").unwrap();
    std::fs::write(root.path().join("beta.rs"), "beta").unwrap();
    let mut app = app();
    app.explorer.root = root.path().into();
    app.chat.input = "@alpha".into();
    app.chat.cursor = app.chat.input.len();
    app.refresh_chat_file_completion();
    app.chat.switch_connection(1);
    app.chat.input = "@beta".into();
    app.chat.cursor = app.chat.input.len();
    app.refresh_chat_file_completion();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app
        .chat
        .agents()
        .any(|(_, state)| state.files.completion.as_ref().is_some_and(|c| c.searching))
    {
        app.poll_background_results();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.chat.active_agent, 1);
    assert_eq!(app.chat.input, "@beta");
    assert_eq!(
        app.chat.files.completion.as_ref().unwrap().paths,
        vec![root.path().join("beta.rs")]
    );
    assert_eq!(
        app.chat
            .agent_state(0)
            .unwrap()
            .files
            .completion
            .as_ref()
            .unwrap()
            .paths,
        vec![root.path().join("alpha.rs")]
    );
}

#[test]
fn failed_reconnect_affects_only_the_selected_agent_and_shows_its_error() {
    let root = tempfile::tempdir().unwrap();
    let mut app = app();
    app.explorer.root = root.path().into();
    app.tabs = vec![Tab::new_file(crate::editor::Editor::new())];
    app.active_tab = 0;
    let id = app.chat.selected.clone().unwrap();
    app.place_chat_in_editor(id.clone(), 0, None);
    let first_key = app.chat.layout.active.unwrap();
    app.select_chat_agent(1);
    app.place_chat_in_editor(id.clone(), 0, None);
    app.chat.agent.as_mut().unwrap().command = root
        .path()
        .join("missing-agent.exe")
        .to_string_lossy()
        .into_owned();
    app.chat_action(ChatAction::Reconnect);
    wait(&mut app, |a| a.chat.error.is_some());
    assert_eq!(app.chat.active_agent, 1);
    assert!(app.chat.layout.active.is_none());
    assert!(app.chat.layout.dock_visible);
    assert_eq!(app.chat.layout.editors.len(), 1);
    assert!(app.chat.layout.editors.contains_key(&first_key));
    assert_eq!(app.chat.agent_state(0).unwrap().input, "OpenCode draft");
    assert!(app.chat.agent_state(0).unwrap().sessions.contains_key(&id));
    let mut terminal = Terminal::new(TestBackend::new(140, 42)).unwrap();
    paint(&mut app, &mut terminal);
    assert!(app.chat.area.is_some());
    assert!(app.chat.error.is_some());
}

fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(25);
    while !done(app) {
        app.poll_acp();
        assert!(
            Instant::now() < deadline,
            "Timed out: {:?}",
            app.chat
                .agents()
                .map(|(id, s)| (id, &s.status, &s.error))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn sdk_config(name: &str) -> AgentConfig {
    let mut config = AgentConfig::new(
        name,
        std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md"),
    );
    config.args = vec![
        "-u".into(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scenario_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    config.env.insert("ACP_TEST_MODEL_A".into(), name.into());
    config
        .env
        .insert("ACP_TEST_CHAT_SELECTORS".into(), "1".into());
    config
}

fn send(app: &mut App, text: &str) {
    app.chat.input = text.into();
    app.chat_action(ChatAction::Send);
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_multiple_agents_keep_permissions_terminals_and_cancel_separate() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.focus_chat();
    app.connect_acp(sdk_config("OpenCode"));
    wait(&mut app, |a| {
        a.chat.selected.is_some() && !a.chat.rollback.busy()
    });
    let id = app.chat.selected.clone().unwrap();
    send(&mut app, "silent");
    wait(&mut app, |a| {
        a.chat
            .diagnostics
            .iter()
            .any(|s| s.contains("Silent prompt received"))
    });
    send(&mut app, "OpenCode queued");
    app.chat.input = "OpenCode draft".into();
    app.connect_acp(sdk_config("Copilot"));
    wait(&mut app, |a| a.chat.selected.is_some());
    assert_eq!(
        app.chat.selected.as_ref(),
        Some(&id),
        "fixture agents deliberately reuse session IDs"
    );
    assert!(app.chat.agent_state(0).unwrap().sessions[&id].busy);
    send(&mut app, "permission");
    wait(&mut app, |a| a.chat.permission().is_some());
    app.select_chat_agent(0);
    assert_eq!(app.chat.input, "OpenCode draft");
    assert!(app.chat.permission().is_none());
    app.chat_action(ChatAction::Stop);
    wait(&mut app, |a| !a.chat.busy());
    assert!(app.chat.agent_state(1).unwrap().sessions[&id].busy);
    // Both connections use the same request ID and session ID, but approvals
    // must go through the client belonging to the selected agent.
    send(&mut app, "permission");
    app.chat_action(ChatAction::PauseQueue);
    wait(&mut app, |a| a.chat.permission().is_some());
    let a_request = app.chat.permission().unwrap().id.clone();
    let b_request = app.chat.agent_state(1).unwrap().permissions[0].id.clone();
    assert_eq!(a_request, b_request);
    let allow = app.chat.permission().unwrap().request.options[0]
        .option_id
        .clone();
    app.chat_action(ChatAction::Permission(a_request, app.chat.permission().unwrap().ticket, Some(allow)));
    wait(&mut app, |a| {
        !a.chat.busy() && a.chat.permissions.is_empty()
    });
    assert_eq!(app.chat.agent_state(1).unwrap().permissions.len(), 1);
    assert!(
        app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::Agent(t) if t.contains("Permission accepted")))
    );
    app.select_chat_agent(1);
    let reject = app.chat.permission().unwrap().request.options[1]
        .option_id
        .clone();
    app.chat_action(ChatAction::Permission(b_request, app.chat.permission().unwrap().ticket, Some(reject)));
    wait(&mut app, |a| !a.chat.busy());
    send(&mut app, "terminal-input");
    wait(&mut app, |a| {
        a.chat
            .session()
            .unwrap()
            .terminals
            .values()
            .any(|t| t.output.contains("ready"))
    });
    let b_terminal = app
        .chat
        .session()
        .unwrap()
        .terminals
        .keys()
        .next()
        .unwrap()
        .clone();
    app.select_chat_agent(0);
    send(&mut app, "terminal-input");
    wait(&mut app, |a| {
        a.chat
            .session()
            .unwrap()
            .terminals
            .values()
            .any(|t| t.output.contains("ready"))
    });
    let a_terminal = app
        .chat
        .session()
        .unwrap()
        .terminals
        .keys()
        .next()
        .unwrap()
        .clone();
    app.chat_action(ChatAction::Terminal(a_terminal));
    send(&mut app, "OpenCode terminal input");
    wait(&mut app, |a| !a.chat.busy());
    assert!(
        app.chat.agent_state(1).unwrap().sessions[&id].terminals[&b_terminal]
            .exit_status
            .is_none()
    );
    app.chat.input = "Preserved after disconnecting other agent".into();
    app.select_chat_agent(1);
    app.chat_action(ChatAction::Disconnect);
    assert!(app.chat.agent_state(0).unwrap().client.is_some());
    app.select_chat_agent(0);
    assert_eq!(app.chat.input, "Preserved after disconnecting other agent");
    send(&mut app, "Still connected");
    wait(&mut app, |a| !a.chat.busy());
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    app.chat_action(ChatAction::Disconnect);
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_parallel_conversations_in_git_and_non_git_workspaces() {
    for git in [true, false] {
        for same_agent in [true, false] {
            let root = tempfile::tempdir().unwrap();
            if git {
                git2::Repository::init(root.path()).unwrap();
            }
            let mut app = App::test_new();
            app.explorer.root = root.path().into();
            app.focus_chat();
            app.connect_acp(sdk_config("OpenCode"));
            wait(&mut app, |a| {
                a.chat.selected.is_some() && !a.chat.rollback.busy()
            });
            let first = app.chat.selected.clone().unwrap();
            if same_agent {
                app.chat_action(ChatAction::New);
                wait(&mut app, |a| {
                    a.chat.selected.as_ref().is_some_and(|id| id != &first)
                });
            } else {
                app.connect_acp(sdk_config("Copilot"));
                wait(&mut app, |a| a.chat.selected.is_some());
            }
            let agent = app.chat.active_agent;
            let second = app.chat.selected.clone().unwrap();
            app.select_chat_agent(0);
            app.chat_action(ChatAction::Switch(first.clone()));
            send(&mut app, "silent");
            // No polling between sends: both before-message captures must be
            // retained even when the first snapshot worker has not replied.
            app.select_chat_agent(agent);
            app.chat_action(ChatAction::Switch(second.clone()));
            assert!(
                !app.chat.checkpoint_pending(),
                "another chat's checkpoint must not affect this composer"
            );
            app.chat.input = "cancel".into();
            let mut terminal = Terminal::new(TestBackend::new(140, 42)).unwrap();
            paint(&mut app, &mut terminal);
            let frame = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(
                !frame.contains("Queue up"),
                "idle conversation must show Send"
            );
            app.chat_action(ChatAction::Send);
            assert!(app.chat.busy());
            assert!(
                !app.chat.has_queued_prompt(),
                "idle chat must send immediately: git={git}, same_agent={same_agent}"
            );
            wait(&mut app, |a| {
                a.chat.session().unwrap().entries.iter().any(|e|
                matches!(e, ChatEntry::Agent(text) if text.contains("Working before cancellation")))
            });
            assert!(app.chat.agent_state(0).unwrap().sessions[&first].busy);
            assert!(
                app.chat
                    .agent_state(0)
                    .unwrap()
                    .diagnostics
                    .iter()
                    .any(|s| s.contains("Silent prompt received"))
            );
            // Queue and force-send affect only the second conversation, while the first stays active.
            send(&mut app, "queue-write:first");
            assert_eq!(app.chat.queues[&second].items.len(), 1);
            let queued = app.chat.queues[&second].items[0].id;
            app.chat_action(ChatAction::SendQueued(queued));
            wait(&mut app, |a| {
                !a.chat.busy() && !a.chat.checkpoint_pending() && a.chat.queues[&second].is_empty()
            });
            assert!(app.chat.agent_state(0).unwrap().sessions[&first].busy);
            assert_eq!(
                std::fs::read_to_string(root.path().join("queue.txt")).unwrap(),
                "first\n"
            );
            send(&mut app, "queue-write:second");
            assert!(app.chat.busy());
            assert!(!app.chat.has_queued_prompt());
            wait(&mut app, |a| !a.chat.busy() && !a.chat.checkpoint_pending());
            assert!(app.chat.agent_state(0).unwrap().sessions[&first].busy);
            assert_eq!(
                std::fs::read_to_string(root.path().join("queue.txt")).unwrap(),
                "first\nsecond\n"
            );
            if git {
                let summary = &app.chat.sessions[&second].rollback;
                assert_eq!(summary.checkpoints.len(), 3);
                assert_eq!(summary.edits.len(), 2);
                assert!(summary.edits.iter().all(|e| {
                    e.conflict
                        .as_ref()
                        .is_some_and(|s| s.contains("concurrently"))
                }));
            }
            app.select_chat_agent(0);
            app.chat_action(ChatAction::Switch(first.clone()));
            app.chat_action(ChatAction::Stop);
            wait(&mut app, |a| {
                !a.chat.any_agent_running() && !a.chat.rollback.busy() && !a.chat.rollback.active
            });
            app.select_chat_agent(agent);
            app.chat_action(ChatAction::Switch(second.clone()));
            if git {
                // Unknown shell ownership must never undo another chat's edits.
                app.chat_action(ChatAction::Resolve(
                    vtcode::acp::rollback::Selection::All,
                    false,
                ));
                wait(&mut app, |a| !a.chat.rollback.busy());
                assert!(
                    app.chat
                        .error
                        .as_ref()
                        .is_some_and(|s| s.contains("concurrently"))
                );
                assert_eq!(
                    std::fs::read_to_string(root.path().join("queue.txt")).unwrap(),
                    "first\nsecond\n"
                );
                // Ordinary non-overlapping turns still allow normal rollback.
                send(&mut app, "queue-write:exclusive");
                wait(&mut app, |a| !a.chat.busy() && !a.chat.checkpoint_pending());
                let edit = app.chat.session().unwrap().rollback.edits.last().unwrap();
                assert!(edit.conflict.is_none());
                app.chat_action(ChatAction::Resolve(
                    vtcode::acp::rollback::Selection::Edit(edit.id),
                    false,
                ));
                wait(&mut app, |a| !a.chat.rollback.busy());
                assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
                assert_eq!(
                    std::fs::read_to_string(root.path().join("queue.txt")).unwrap(),
                    "first\nsecond\n"
                );
            }
            app.chat_action(ChatAction::Disconnect);
            if !same_agent {
                app.select_chat_agent(0);
                app.chat_action(ChatAction::Disconnect);
            }
        }
    }
}
