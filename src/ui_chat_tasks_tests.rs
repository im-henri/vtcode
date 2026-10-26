use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use vtcode::acp::TaskStatus;

fn setup() -> App {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    *app.chat.session_mut().unwrap() = Session::new(id);
    app
}

fn plan(app: &mut App, completed: usize, total: usize) {
    let entries: Vec<_> = (0..total)
        .map(|i| {
            json!({
                "content":format!("Task {i:02}: inspect the code and check the result"),
                "priority":"medium", "status":if i < completed { "completed" }
                    else if i == completed { "in_progress" } else { "pending" }
            })
        })
        .collect();
    app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
        app.chat.selected.clone().unwrap(),
        serde_json::from_value(json!({"sessionUpdate":"plan", "entries":entries})).unwrap(),
    )));
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) -> String {
    app.text_input_regions.clear();
    terminal.draw(|f| draw(f, app, f.area())).unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn click(app: &mut App, action: ChatAction) {
    let hit = app
        .chat
        .hits
        .iter()
        .find(|hit| hit.action == action)
        .unwrap()
        .area;
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.x,
        row: hit.y,
        modifiers: KeyModifiers::empty(),
    });
}

#[test]
fn tasks_dropdown_stays_above_composer_and_follows_live_status_while_reading_history() {
    let mut app = setup();
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Work on the code".into());
    plan(&mut app, 1, 6);
    for i in 0..60 {
        app.chat
            .session_mut()
            .unwrap()
            .push(ChatEntry::Agent(format!("Output {i}")));
    }
    let mut terminal = Terminal::new(TestBackend::new(88, 32)).unwrap();
    app.chat.output_detail = OutputDetail::Minimal;
    app.chat.follow = false;
    app.chat.scroll = 0;
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("TODOs 1/6"));
    assert!(text.contains("Task 01:"));
    let header = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == ChatAction::Picker(Picker::Tasks))
        .unwrap()
        .area;
    assert_eq!(header.height, 1);
    let input = app.text_input_regions.clone();
    let input_row = text
        .lines()
        .position(|line| line.contains("Ask anything"))
        .unwrap() as u16;
    assert_eq!(header.bottom() + 1, input_row);
    assert_eq!(app.chat.transcript_area.unwrap().bottom(), header.y);
    let scroll = app.chat.scroll;
    plan(&mut app, 2, 6);
    assert!(render(&mut app, &mut terminal).contains("TODOs 2/6"));
    assert_eq!(app.chat.scroll, scroll);
    assert!(!app.chat.follow);
    presentation_tests::capture(&terminal, "tasks-pinned");
    click(&mut app, ChatAction::Picker(Picker::Tasks));
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("▾ TODOs 2/6"));
    assert!(text.contains("Task 02:"));
    assert_eq!(tasks::height(&app), 1);
    let first_task = app
        .chat
        .hits
        .iter()
        .find(|hit| hit.action == ChatAction::Pick(0))
        .unwrap()
        .area;
    assert!(first_task.bottom() <= header.y);
    assert_eq!(app.text_input_regions, input);
    assert!(
        app.chat
            .hits
            .iter()
            .filter(|hit| matches!(hit.action, ChatAction::Pick(_)))
            .all(|hit| hit.area.bottom() <= header.y)
    );
    presentation_tests::capture(&terminal, "tasks-open");
    click(&mut app, ChatAction::Picker(Picker::Tasks));
    assert!(app.chat.picker.is_none());
    assert!(render(&mut app, &mut terminal).contains("▸ TODOs 2/6"));
    assert_eq!(app.chat.scroll, scroll);
    assert_eq!(app.text_input_regions, input);
    presentation_tests::capture(&terminal, "tasks-collapsed");
}

#[test]
fn tasks_cursor_report_pins_above_input_and_expands_as_a_clean_live_checklist() {
    let mut app = setup();
    let report: p::ToolCall =
        serde_json::from_str(include_str!("../tests/fixtures/acp-update-todos.json")).unwrap();
    app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
        app.chat.selected.clone().unwrap(),
        p::SessionUpdate::ToolCall(report),
    )));
    app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
        app.chat.selected.clone().unwrap(),
        serde_json::from_value(json!({
            "sessionUpdate":"tool_call_update", "toolCallId":"cursor-todos", "status":"completed"
        }))
        .unwrap(),
    )));
    let mut terminal = Terminal::new(TestBackend::new(68, 34)).unwrap();
    for detail in [
        OutputDetail::Minimal,
        OutputDetail::Compact,
        OutputDetail::Full,
    ] {
        app.chat.output_detail = detail;
        let text = render(&mut app, &mut terminal);
        assert!(text.contains("TODOs 0/4 · ● Getting your pet ready."));
        assert!(
            !text.contains("TODO_STATUS")
                && !text.contains("Created at:")
                && !text.contains("tool name:")
        );
        let input_row = text
            .lines()
            .position(|line| line.contains("Ask anything"))
            .unwrap() as u16;
        let toggle = app
            .chat
            .hits
            .iter()
            .find(|hit| hit.action == ChatAction::Picker(Picker::Tasks))
            .unwrap()
            .area;
        assert_eq!(toggle.bottom() + 1, input_row);
        presentation_tests::capture(&terminal, &format!("tasks-cursor-{}", detail.label()));
    }
    click(&mut app, ChatAction::Picker(Picker::Tasks));
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("● Getting your pet ready."));
    assert!(text.contains("○ Hatching your pet."));
    assert_eq!(app.chat.choices.len(), 4);
    let first = app
        .chat
        .hits
        .iter()
        .find(|hit| hit.action == ChatAction::Pick(0))
        .unwrap()
        .area;
    assert!(
        first.width > 52,
        "the expanded checklist should use the chat's width"
    );
    presentation_tests::capture(&terminal, "tasks-cursor-expanded");
    app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
        app.chat.selected.clone().unwrap(),
        serde_json::from_value(json!({
            "sessionUpdate":"tool_call_update", "toolCallId":"cursor-todos",
            "rawOutput":{"tool_name":"updateTodos","todos":[
                {"content":"Getting your pet ready.", "status":"TODO_STATUS_COMPLETED"},
                {"content":"Imagining your pet's main look.", "status":"TODO_STATUS_IN_PROGRESS"}]}
        }))
        .unwrap(),
    )));
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("TODOs 1/2 · ● Imagining your pet's main look."));
    assert_eq!(app.chat.choices.len(), 2);
    assert_eq!(app.chat.picker, Some(Picker::Tasks));
    click(&mut app, ChatAction::Picker(Picker::Tasks));
    render(&mut app, &mut terminal);
    assert!(app.chat.picker.is_none());
}

#[test]
fn tasks_strip_keeps_its_place_with_queued_prompts_permissions_errors_and_resize() {
    use crate::app_acp::queue::{PromptQueue, QueuedPrompt};

    let mut app = setup();
    let id = app.chat.selected.clone().unwrap();
    plan(&mut app, 1, 15);
    app.chat.session_mut().unwrap().busy = true;
    app.chat.input = "Keep this draft".into();
    app.chat.cursor = app.chat.input.len();
    app.chat.queues.insert(
        id.clone(),
        PromptQueue {
            items: vec![QueuedPrompt {
                id: 1,
                text: "Next queued message".into(),
                files: Vec::new(),
                paste: None,
            }]
            .into(),
            ..Default::default()
        },
    );
    app.apply_acp_event(vtcode::acp::Event::Permission {
        ticket: 0,
        id: serde_json::from_value(json!(17)).unwrap(),
        request: serde_json::from_value(json!({"sessionId":id,
            "toolCall":{"toolCallId":"search", "title":"grep", "kind":"search",
                "rawInput":{"pattern":"TODO", "path":"src"}},
            "options":[{"optionId":"yes", "name":"Allow once", "kind":"allow_once"},
                {"optionId":"no", "name":"Reject", "kind":"reject_once"}]}))
        .unwrap(),
    });
    app.chat.error = Some("Example connection error".into());
    let mut terminal = Terminal::new(TestBackend::new(88, 48)).unwrap();
    for (width, height) in [(88, 48), (42, 44), (88, 48)] {
        terminal.backend_mut().resize(width, height);
        terminal.resize(Rect::new(0, 0, width, height)).unwrap();
        let text = render(&mut app, &mut terminal);
        let toggle = app
            .chat
            .hits
            .iter()
            .find(|hit| hit.action == ChatAction::Picker(Picker::Tasks))
            .unwrap()
            .area;
        let draft = text
            .lines()
            .position(|line| line.contains("Keep this draft"))
            .unwrap() as u16;
        assert_eq!(toggle.height, 1);
        assert_eq!(toggle.bottom() + 1, draft);
        assert!(text.contains("Queued (1)"));
        assert!(text.contains("Permission needed"));
        assert!(text.contains("Example connection error"));
        assert!(
            app.chat
                .hits
                .iter()
                .filter(|hit| matches!(
                    hit.action,
                    ChatAction::Permission(..)
                        | ChatAction::EditQueued(_)
                        | ChatAction::RemoveQueued(_)
                ))
                .all(|hit| hit.area.bottom() <= toggle.y)
        );
        let inputs = app.text_input_regions.clone();
        click(&mut app, ChatAction::Picker(Picker::Tasks));
        render(&mut app, &mut terminal);
        assert_eq!(app.text_input_regions, inputs);
        assert!(
            app.chat
                .hits
                .iter()
                .filter(|hit| matches!(hit.action, ChatAction::Pick(_)))
                .all(|hit| hit.area.bottom() <= toggle.y)
        );
        let bar = app
            .chat
            .scrollbars
            .iter()
            .find(|bar| bar.picker == Some(Picker::Tasks))
            .unwrap()
            .area;
        app.handle_chat_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: bar.right() - 1,
            row: bar.bottom() - 1,
            modifiers: KeyModifiers::empty(),
        });
        assert!(app.chat.picker_scroll > 0);
        app.handle_chat_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: bar.right() - 1,
            row: bar.bottom() - 1,
            modifiers: KeyModifiers::empty(),
        });
        render(&mut app, &mut terminal);
        assert!(
            app.chat
                .hits
                .iter()
                .any(|hit| hit.action == ChatAction::Pick(14))
        );
        presentation_tests::capture(&terminal, &format!("tasks-queued-permission-{width}"));
        click(&mut app, ChatAction::Picker(Picker::Tasks));
        assert_eq!(app.chat.input, "Keep this draft");
        assert_eq!(
            app.chat.permissions.len(),
            1,
            "opening TODOs must not grant permission"
        );
        assert_eq!(app.chat.queues[&id].items.len(), 1);
    }
}

#[test]
fn tasks_full_list_scrolls_and_remains_live_without_mutating_agent_tasks() {
    let mut app = setup();
    plan(&mut app, 2, 15);
    let mut terminal = Terminal::new(TestBackend::new(88, 36)).unwrap();
    render(&mut app, &mut terminal);
    click(&mut app, ChatAction::Picker(Picker::Tasks));
    assert_eq!(app.chat.picker_selected, 2);
    assert_eq!(app.chat.choices.len(), 15);
    assert!(render(&mut app, &mut terminal).contains("In progress · Medium priority"));
    click(&mut app, ChatAction::Pick(2));
    assert_eq!(app.chat.picker, Some(Picker::Tasks));
    assert_eq!(
        app.chat.session().unwrap().tasks.as_ref().unwrap().entries[2].status,
        TaskStatus::InProgress
    );
    for _ in 0..12 {
        app.handle_chat_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    }
    assert!(render(&mut app, &mut terminal).contains("Task 14:"));
    assert!(app.chat.picker_scroll > 0);
    plan(&mut app, 15, 15);
    assert_eq!(app.chat.picker_selected, 14);
    assert!(render(&mut app, &mut terminal).contains("Completed · Medium priority"));
    presentation_tests::capture(&terminal, "tasks-all");
    app.handle_chat_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
    assert!(app.chat.picker.is_none());
    app.chat_action(ChatAction::Picker(Picker::Tasks));
    plan(&mut app, 0, 0);
    assert!(app.chat.picker.is_none());
    assert!(app.chat.choices.is_empty());
}

#[test]
fn tasks_dropdown_and_list_are_isolated_between_conversations() {
    let mut app = setup();
    plan(&mut app, 1, 4);
    let first = app.chat.selected.clone().unwrap();
    app.chat_action(ChatAction::Picker(Picker::Tasks));
    assert_eq!(app.chat.choices.len(), 4);
    let second = p::SessionId::new("second");
    app.chat
        .sessions
        .insert(second.clone(), Session::new(second.clone()));
    app.chat.select_tab(Some(second.clone()));
    assert!(app.chat.picker.is_none());
    assert_eq!(tasks::height(&app), 0);
    plan(&mut app, 0, 2);
    app.chat.select_tab(Some(first.clone()));
    assert!(app.chat.picker.is_none());
    app.chat_action(ChatAction::Picker(Picker::Tasks));
    assert_eq!(app.chat.choices.len(), 4);
    assert_eq!(
        app.chat
            .session()
            .unwrap()
            .tasks
            .as_ref()
            .unwrap()
            .completed(),
        1
    );
    assert_eq!(
        app.chat.sessions[&second]
            .tasks
            .as_ref()
            .unwrap()
            .completed(),
        0
    );
    app.chat.select_tab(Some(second));
    assert!(app.chat.picker.is_none());
    app.chat_action(ChatAction::Picker(Picker::Tasks));
    assert_eq!(app.chat.choices.len(), 2);
}

#[test]
fn tasks_tool_cards_use_checklists_instead_of_raw_todo_json() {
    let mut app = setup();
    app.chat.session_mut().unwrap().apply(serde_json::from_value(json!({
        "sessionUpdate":"tool_call", "toolCallId":"tasks", "title":"todowrite", "status":"completed",
        "rawInput":{"todos":[{"content":"Inspect workspace", "status":"completed", "priority":"high"},
            {"content":"Run checks", "status":"in_progress", "priority":"medium"}]}
    })).unwrap());
    for detail in [
        OutputDetail::Minimal,
        OutputDetail::Compact,
        OutputDetail::Full,
    ] {
        let rows = entry_rows_detail(
            app.chat.session().unwrap(),
            0,
            50,
            &app.colors,
            detail == OutputDetail::Full,
            "Local agent",
            None,
            detail,
        );
        let text: String = rows.iter().map(|row| row.line.to_string()).collect();
        assert!(text.contains("TODOs 1/2"));
        assert!(
            !text.contains("content:") && !text.contains("rawInput") && !text.contains("priority")
        );
        assert_eq!(text.contains("Run checks"), detail != OutputDetail::Minimal);
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 28)).unwrap();
    app.chat.output_detail = OutputDetail::Minimal;
    let text = render(&mut app, &mut terminal);
    assert!(text.contains("Run checks"));
    presentation_tests::capture(&terminal, "tasks-tool-minimal");
    click(&mut app, ChatAction::Picker(Picker::Tasks));
    assert!(render(&mut app, &mut terminal).contains("✓ Inspect workspace"));
}

#[test]
fn tasks_empty_snapshot_removes_the_panel_and_tiny_panes_preserve_composer() {
    let mut app = setup();
    app.chat.input = "Draft message".into();
    app.chat.cursor = app.chat.input.len();
    let mut terminal = Terminal::new(TestBackend::new(24, 14)).unwrap();
    render(&mut app, &mut terminal);
    assert!(
        !app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::Picker(Picker::Tasks))
    );
    plan(&mut app, 0, 9);
    let text = render(&mut app, &mut terminal);
    for hit in &app.chat.hits {
        assert!(hit.area.right() <= 24 && hit.area.bottom() <= 14);
    }
    assert!(text.contains("Send"));
    plan(&mut app, 0, 0);
    assert_eq!(tasks::height(&app), 0);
    render(&mut app, &mut terminal);
    assert!(
        !app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::Picker(Picker::Tasks))
    );
}

#[test]
fn tasks_dock_and_editor_surfaces_render_their_own_lists_and_dropdowns() {
    let mut app = setup();
    app.show_explorer = false;
    app.show_panel = false;
    app.chat.width = 65;
    let first = app.chat.selected.clone().unwrap();
    plan(&mut app, 1, 4);
    let second = p::SessionId::new("dock-session");
    app.chat
        .sessions
        .insert(second.clone(), Session::new(second.clone()));
    app.chat.open_tabs.push(second.clone());
    app.chat.select_tab(Some(second.clone()));
    plan(&mut app, 0, 2);
    app.place_chat_in_editor(first.clone(), app.active_group, None);
    let mut terminal = Terminal::new(TestBackend::new(170, 42)).unwrap();
    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    let surfaces = app.chat.layout.surfaces.clone();
    for (id, label) in [(&first, "TODOs 1/4"), (&second, "TODOs 0/2")] {
        let surface = surfaces
            .iter()
            .find(|surface| surface.session.as_ref() == Some(id))
            .unwrap();
        let area = surface
            .hits
            .iter()
            .find(|hit| hit.action == ChatAction::Picker(Picker::Tasks))
            .unwrap()
            .area;
        let text: String = (area.x..area.right())
            .map(|x| terminal.backend().buffer()[(x, area.y)].symbol())
            .collect();
        assert!(text.contains(label));
    }
    let area = surfaces
        .iter()
        .find(|surface| surface.session.as_ref() == Some(&first))
        .unwrap()
        .hits
        .iter()
        .find(|hit| hit.action == ChatAction::Picker(Picker::Tasks))
        .unwrap()
        .area;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::empty(),
        },
    );
    assert_eq!(app.chat.selected.as_ref(), Some(&first));
    assert_eq!(app.chat.picker, Some(Picker::Tasks));
    assert_eq!(app.chat.choices.len(), 4);
    app.chat.select_tab(Some(second));
    assert!(app.chat.picker.is_none());
}
