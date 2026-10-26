use super::*;
use crate::app::TabKind;
use crate::app_acp::queue::{PromptQueue, QueuedPrompt};

fn modal_button(app: &mut App, index: usize) {
    let area = app
        .modal
        .buttons
        .iter()
        .find(|(_, button)| *button == index)
        .unwrap()
        .0;
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 1,
            row: area.y,
            modifiers: KeyModifiers::empty(),
        },
    );
}

#[test]
fn close_agent_confirmation_counts_editor_chats_and_cancel_preserves_everything() {
    for (width, height) in [(170, 45), (48, 25)] {
        let mut app = app();
        let id = app.chat.selected.clone().unwrap();
        app.chat.session_mut().unwrap().busy = true;
        app.place_chat_in_editor(id.clone(), 0, None);
        let key = app.chat.layout.active.unwrap();
        app.select_chat_agent(1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        paint(&mut app, &mut terminal);
        if !app
            .chat
            .hits
            .iter()
            .any(|hit| hit.action == ChatAction::CloseAgent(0))
        {
            click_surface(&mut app, None, ChatAction::AgentPage(true));
            paint(&mut app, &mut terminal);
        }
        // An inactive agent's close button must keep its own identity.
        click_surface(&mut app, None, ChatAction::CloseAgent(0));
        paint(&mut app, &mut terminal);
        assert_eq!(app.chat.confirm_close_agent, Some(0));
        assert_eq!(app.modal.focus, 1, "Cancel is the initial keyboard choice");
        let frame = text(&terminal);
        assert!(frame.contains("Close OpenCode?"));
        assert!(frame.contains("1 open conversation"));
        assert!(frame.contains("1 running response"));
        assert_eq!(app.modal.buttons.len(), 2);
        capture(&terminal, &format!("close-confirm-{width}"));
        crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.chat.confirm_close_agent.is_none());
        assert_eq!(app.chat.agent_order, vec![0, 1]);
        assert!(app.chat.agent_state(0).unwrap().sessions[&id].busy);
        assert!(app.chat.layout.editors.contains_key(&key));
        assert_eq!(app.chat.active_agent, 1);
        assert_eq!(app.chat.input, "Copilot draft");
        for key in [KeyCode::Esc, KeyCode::Enter] {
            app.chat_action(ChatAction::CloseAgent(0));
            paint(&mut app, &mut terminal);
            if key == KeyCode::Esc {
                crate::ui_modal::handle_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE));
            } else {
                modal_button(&mut app, 1);
            }
            assert!(app.chat.confirm_close_agent.is_none());
            assert_eq!(app.chat.agent_order.len(), 2);
        }
        app.chat.switch_connection(0);
        assert!(app.activate_editor_chat(&id));
        assert_eq!(app.chat.input, "OpenCode draft");
    }
}

#[test]
fn close_agent_removes_only_its_tabs_and_preserves_other_agents_work() {
    let mut app = app();
    let id = app.chat.selected.clone().unwrap();
    app.settings_effective.acp_agents = Some(vec![app.chat.agent.clone().unwrap()]);
    app.tabs = vec![Tab::new_file(crate::editor::Editor::test_with_text(
        "unsaved file",
    ))];
    app.tabs[0].editor.modified = true;
    let file = app.tabs[0].editor.navigation_id;
    app.active_tab = 0;
    app.place_chat_in_editor(id.clone(), 0, Some(TabDropZone::Right));
    let first = app.chat.layout.active.unwrap();
    app.select_chat_agent(1);
    app.chat.session_mut().unwrap().busy = true;
    app.chat.queues.insert(
        id.clone(),
        PromptQueue {
            items: [QueuedPrompt {
                id: 1,
                text: "Keep queued".into(),
                files: vec![],
                paste: None,
            }]
            .into(),
            ..Default::default()
        },
    );
    app.place_chat_in_editor(id.clone(), 0, None);
    let second = app.chat.layout.active.unwrap();
    let mut terminal = Terminal::new(TestBackend::new(180, 55)).unwrap();
    app.chat_action(ChatAction::CloseAgent(0));
    paint(&mut app, &mut terminal);
    modal_button(&mut app, 0);
    paint(&mut app, &mut terminal);
    assert_eq!(app.chat.agent_order, vec![1]);
    assert!(app.chat.agent_state(0).is_none());
    assert_eq!(app.chat.active_agent, 1);
    assert_eq!(app.chat.layout.active, Some(second));
    assert!(!app.chat.layout.editors.contains_key(&first));
    assert!(app.chat.layout.editors.contains_key(&second));
    assert!(app.chat.sessions[&id].busy);
    assert_eq!(app.chat.input, "Copilot draft");
    assert_eq!(app.chat.queues[&id].items[0].text, "Keep queued");
    let tabs: Vec<_> = app
        .tabs
        .iter()
        .chain(app.groups.iter().flat_map(|g| &g.tabs))
        .collect();
    assert!(
        tabs.iter()
            .any(|tab| tab.editor.navigation_id == file && tab.editor.modified)
    );
    assert!(tabs.iter().all(|tab| tab.kind != TabKind::Chat(first)));
    assert_eq!(app.settings_effective.acp_agents.as_ref().unwrap().len(), 1);
}

#[test]
fn close_agent_middle_click_and_last_agent_return_to_the_empty_picker() {
    let mut app = app();
    app.select_chat_agent(1);
    let mut terminal = Terminal::new(TestBackend::new(160, 45)).unwrap();
    paint(&mut app, &mut terminal);
    let tab = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == ChatAction::SwitchAgent(1))
        .unwrap()
        .area;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Middle),
            column: tab.x + 3,
            row: tab.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert_eq!(app.chat.confirm_close_agent, Some(1));
    paint(&mut app, &mut terminal);
    modal_button(&mut app, 0);
    paint(&mut app, &mut terminal);
    assert_eq!(app.chat.active_agent, 0);
    assert_eq!(app.chat.input, "OpenCode draft");
    click_surface(&mut app, None, ChatAction::CloseAgent(0));
    paint(&mut app, &mut terminal);
    modal_button(&mut app, 0);
    paint(&mut app, &mut terminal);
    assert!(app.chat.agent_order.is_empty());
    assert!(app.chat.connections.is_empty());
    assert!(app.chat.agent.is_none());
    assert!(app.chat.active_agent > 1);
    click(&mut app, ChatAction::Picker(Picker::Agents));
    assert!(
        app.chat
            .choices
            .iter()
            .any(|choice| choice.action == ChatAction::Configure)
    );
    app.chat_action(ChatAction::CloseAgent(0));
    assert!(
        app.chat.confirm_close_agent.is_none(),
        "stale close target is ignored"
    );
}

#[test]
fn close_agent_without_conversations_needs_no_confirmation_but_hidden_drafts_do() {
    let mut app = app();
    app.chat.connections.insert(
        2,
        AgentState {
            agent: Some(AgentConfig::new("Unused", "unused-agent")),
            ..Default::default()
        },
    );
    app.chat.agent_order.push(2);
    app.chat.next_agent = 3;
    app.chat_action(ChatAction::CloseAgent(2));
    assert!(app.chat.agent_state(2).is_none());
    assert!(app.chat.confirm_close_agent.is_none());
    let id = app.chat.selected.clone().unwrap();
    app.chat.close_tab(&id);
    assert_eq!(app.chat.agent_open_conversations(0), 0);
    assert!(app.chat.input.is_empty());
    app.chat_action(ChatAction::CloseAgent(0));
    assert_eq!(app.chat.confirm_close_agent, Some(0));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn close_agent_sdk_preserves_other_running_agents_and_can_reopen_after_checkpoint_capture() {
    for git in [true, false] {
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
        send(&mut app, "silent");
        wait(&mut app, |a| {
            a.chat
                .diagnostics
                .iter()
                .any(|s| s.contains("Silent prompt received"))
        });
        app.connect_acp(sdk_config("Copilot"));
        wait(&mut app, |a| a.chat.selected.is_some());
        let id = app.chat.selected.clone().unwrap();
        send(&mut app, "terminal-input");
        wait(&mut app, |a| {
            a.chat
                .session()
                .unwrap()
                .terminals
                .values()
                .any(|t| t.output.contains("ready"))
        });
        let terminal = app
            .chat
            .session()
            .unwrap()
            .terminals
            .keys()
            .next()
            .unwrap()
            .clone();
        app.chat.input = "Copilot draft".into();
        app.chat_action(ChatAction::CloseAgent(0));
        assert_eq!(app.chat.confirm_close_agent, Some(0));
        app.confirm_close_chat_agent();
        wait(&mut app, |a| a.chat.agent_state(0).is_none());
        assert_eq!(app.chat.agent_order, vec![1]);
        assert_eq!(app.chat.active_agent, 1);
        assert_eq!(app.chat.input, "Copilot draft");
        assert!(app.chat.busy());
        assert!(
            app.chat.sessions[&id].terminals[&terminal]
                .exit_status
                .is_none()
        );
        app.chat_action(ChatAction::Terminal(terminal));
        send(&mut app, "Still running");
        wait(&mut app, |a| {
            !a.chat.busy() && !a.chat.rollback.agent_busy(1)
        });
        assert_eq!(app.chat.input, "Copilot draft");
        app.chat_action(ChatAction::CloseAgent(1));
        app.confirm_close_chat_agent();
        wait(&mut app, |a| a.chat.agent_order.is_empty());
        app.connect_acp(sdk_config("OpenCode"));
        wait(&mut app, |a| {
            a.chat.selected.is_some() && !a.chat.rollback.busy()
        });
        assert!(app.chat.active_agent > 1);
        send(&mut app, "After reopening");
        wait(&mut app, |a| !a.chat.busy());
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        let agent = app.chat.active_agent;
        app.chat_action(ChatAction::CloseAgent(agent));
        app.confirm_close_chat_agent();
        wait(&mut app, |a| a.chat.agent_order.is_empty());
    }
}
