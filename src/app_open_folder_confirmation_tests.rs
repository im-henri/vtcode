use super::*;
use crate::app::{EditorGroup, SplitDirection, TabKind};
use vtcode::acp::{Session, protocol as p};

fn chat_session(app: &mut App, name: &str, running: bool) -> p::SessionId {
    let id = p::SessionId::new(name);
    let mut session = Session::new(id.clone());
    session.begin_prompt("A request".into());
    if !running {
        session.finish(Some(p::StopReason::EndTurn));
    }
    app.chat.sessions.insert(id.clone(), session);
    app.chat.session_order.push(id.clone());
    app.chat.open_tabs.push(id.clone());
    app.chat.selected = Some(id.clone());
    id
}

fn cancel(app: &mut App) {
    crate::ui_workspaces::handle_key(app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.workspace_set_switching());
}

#[test]
fn clean_folders_saved_files_idle_chats_and_desks_open_without_confirmation() {
    for state in 0..5 {
        let source = Project::new();
        let destination = Project::new();
        let mut app = source.app(Default::default());
        let file = source.path().join("saved.txt");
        std::fs::write(&file, "saved text").unwrap();
        app.editor_mut().unwrap().open_file(&file).unwrap();
        match state {
            0 => app.tabs.clear(),
            1 => {}
            2 => {
                app.chat.visible = true;
                app.chat.agent_order.push(0);
                chat_session(&mut app, "completed", false);
            }
            3 => {
                app.new_desk("Other desk", false);
            }
            _ => {
                app.create_split_group_dir(0, SplitDirection::Horizontal);
            }
        }
        assert_eq!(app.unsaved_file_count(), 0);
        assert!(!app.chat.any_agent_running());
        assert!((0..app.terminals.len()).all(|index| !app.terminal_is_open(index)));
        choose(&mut app, destination.path());
        finish_without_confirmation(&mut app);
        assert_eq!(app.explorer.root, destination.path());
        assert_eq!(app.open_folder_count(), 1);
        assert!(app.workspaces.parked.is_empty());
    }
}

#[test]
fn clean_parked_folders_do_not_require_confirmation() {
    let a = Project::new();
    let b = Project::new();
    let destination = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    choose(&mut app, destination.path());
    finish_without_confirmation(&mut app);
    assert_eq!(app.explorer.root, destination.path());
    assert_eq!(app.open_folder_count(), 1);
    assert!(app.workspaces.parked.is_empty());
}

#[test]
fn unsaved_changes_in_any_pane_desk_or_folder_still_require_confirmation() {
    for state in 0..5 {
        let source = Project::new();
        let parked = Project::new();
        let destination = Project::new();
        let wake = crate::background::UiWake::default();
        let mut app = source.app(wake.clone());
        if state == 1 {
            let mut group = EditorGroup::new();
            let mut editor = Editor::test_with_text("other pane");
            editor.insert_char('!');
            group.tabs.push(Tab::new_file(editor));
            app.groups.push(group);
        } else {
            app.editor_mut().unwrap().insert_char('!');
            match state {
                2 => {
                    app.new_desk("Other desk", false);
                }
                3 => {
                    app.insert_workspace(parked.app(wake));
                }
                4 => app.tabs[0].kind = TabKind::ExtensionView,
                _ => {}
            }
        }
        let count = app.open_folder_count();
        choose(&mut app, destination.path());
        confirmation(&mut app);
        assert_eq!(app.open_folder_count(), count);
        cancel(&mut app);
        assert!(app.workspace_unsaved_file_count() > 0);
    }
}

#[test]
fn hidden_unlaunched_and_editor_terminals_still_require_confirmation() {
    for state in 0..4 {
        let source = Project::new();
        let parked = Project::new();
        let destination = Project::new();
        let wake = crate::background::UiWake::default();
        let mut app = source.app(wake.clone());
        let mut terminal = TerminalTab::new("Shell".into(), source.path().into());
        let running = if state == 1 {
            let mut pty = TerminalEmulator::test_with_output(40, 8, "shell");
            let running = pty.test_keep_running();
            terminal.terminal_pty = Some(pty);
            Some(running)
        } else {
            None
        };
        app.terminals = vec![terminal];
        if state == 2 {
            assert!(app.detach_terminal_to_group(0, 0));
        }
        if state == 3 {
            app.insert_workspace(parked.app(wake));
        }
        choose(&mut app, destination.path());
        confirmation(&mut app);
        cancel(&mut app);
        drop(running);
    }
}

#[test]
fn running_pending_and_background_agent_chats_still_require_confirmation() {
    for state in 0..6 {
        let source = Project::new();
        let parked = Project::new();
        let destination = Project::new();
        let wake = crate::background::UiWake::default();
        let mut app = source.app(wake.clone());
        let id = chat_session(&mut app, "work", matches!(state, 0 | 1 | 5));
        match state {
            1 => { chat_session(&mut app, "selected idle chat", false); }
            2 => app.chat.control_busy = true,
            3 => app.chat.permissions.push_back(crate::app_acp::Permission {
                id: p::RequestId::Number(1), ticket: 1, tool_title: None,
                request: serde_json::from_value(serde_json::json!({
                    "sessionId": "work", "toolCall": {"toolCallId": "tool", "title": "Run command"}, "options": []
                })).unwrap(),
            }),
            4 => app.chat.queues.entry(id).or_default().items.push_back(crate::app_acp::queue::QueuedPrompt {
                id: 1, text: "Pending request".into(), files: vec![], paste: None,
            }),
            5 => { app.insert_workspace(parked.app(wake)); }
            _ => {},
        }
        choose(&mut app, destination.path());
        confirmation(&mut app);
        cancel(&mut app);
        assert_eq!(app.open_folder_count(), if state == 5 { 2 } else { 1 });
    }
}

#[test]
fn work_created_during_automatic_validation_requires_a_real_confirmation() {
    let source = Project::new();
    let destination = Project::new();
    let mut app = source.app(Default::default());
    choose(&mut app, destination.path());
    until(|| {
        tick(&mut app);
        app.workspace_set_preserving()
    });
    assert!(!matches!(
        app.workspaces.popup.sets.mode,
        Some(crate::ui_workspace_sets::Mode::Confirm)
    ));
    app.editor_mut().unwrap().insert_char('!');
    confirmation(&mut app);
    assert_eq!(app.explorer.root, source.path());
    assert_eq!(app.editor().unwrap().rope.to_string(), "!original");
    cancel(&mut app);
}

#[test]
fn skipping_the_warning_still_validates_the_destination_before_closing() {
    let source = Project::new();
    let destination = Project::new();
    let mut app = source.app(Default::default());
    let identity = app.editor().unwrap().save_identity;
    choose(&mut app, destination.path());
    app.poll_workspace_requests();
    app.show_save_error("Hold the prepared destination before validation");
    until(|| {
        tick(&mut app);
        app.workspace_set_confirmation().is_some()
    });
    std::fs::remove_dir(destination.path()).unwrap();
    app.dismiss_save_error();
    finish_without_confirmation(&mut app);
    assert_eq!(app.explorer.root, source.path());
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert_eq!(app.open_folder_count(), 1);
}
