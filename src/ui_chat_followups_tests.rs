use super::*;
use crate::app_acp::queue::{PromptQueue, QueuedPrompt};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.text_input_regions.clear();
    terminal.draw(|f| draw(f, app, f.area())).unwrap();
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
fn editor_chat_global_button_preserves_conversation_and_explicit_return_preserves_draft() {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    app.chat.input = "Keep my draft".into();
    app.chat.cursor = app.chat.input.len();
    app.chat.session_mut().unwrap().busy = true;
    let entries = app.chat.session().unwrap().entries.len();
    app.place_chat_in_editor(id.clone(), 0, None);
    let key = app.chat.layout.active.unwrap();
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    for visible in [true, false, true] {
        app.execute_acp_command("acp_chat");
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(
            app.chat.layout.surfaces.iter().any(|s| s.key.is_none()),
            visible
        );
    }
    assert!(
        app.tabs
            .iter()
            .any(|tab| tab.kind == crate::app::TabKind::Chat(key))
    );
    assert!(app.activate_editor_chat(&id));
    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
    assert!(
        app.chat
            .hits
            .iter()
            .any(|hit| hit.action == ChatAction::MoveToDock)
    );
    app.chat_action(ChatAction::MoveToDock);
    assert!(
        !app.tabs
            .iter()
            .any(|tab| tab.kind == crate::app::TabKind::Chat(key))
    );
    assert!(app.chat.open_tabs.contains(&id));
    assert_eq!(app.chat.input, "Keep my draft");
    assert_eq!(app.chat.session().unwrap().entries.len(), entries);
    assert!(app.chat.session().unwrap().busy);
    assert!(app.chat.layout.active.is_none());
}

#[test]
fn permission_card_shows_merged_task_inputs_and_full_request_is_readable() {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    *app.chat.session_mut().unwrap() = Session::new(id.clone());
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Explore the UI".into());
    let tool: p::ToolCall = serde_json::from_value(serde_json::json!({
        "toolCallId":"task-1", "title":"task", "kind":"other", "status":"pending",
        "rawInput":{"description":"Inspect chat rendering", "subagent_type":"explore", "prompt":"Find the permission rendering code.\nReport all affected files and their responsibilities."}
    })).unwrap();
    app.chat
        .session_mut()
        .unwrap()
        .apply(p::SessionUpdate::ToolCall(tool));
    app.apply_acp_event(vtcode::acp::Event::Permission {
        ticket: 0,
        id: serde_json::from_value(serde_json::json!(17)).unwrap(),
        request: serde_json::from_value(serde_json::json!({"sessionId": id, "toolCall":{"toolCallId":"task-1", "title":"task"},
            "options":[{"optionId":"yes", "name":"Allow once", "kind":"allow_once"}, {"optionId":"no", "name":"Reject", "kind":"reject_once"}]})).unwrap(),
    });
    let mut terminal = Terminal::new(TestBackend::new(92, 40)).unwrap();
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Inspect chat rendering"));
    assert!(text(&terminal).contains("View full request details"));
    app.chat_action(ChatAction::PermissionDetails);
    render(&mut app, &mut terminal);
    let transcript: String = app
        .chat
        .cache
        .rows
        .iter()
        .flat_map(|r| r.line.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(transcript.contains("Subagent type: explore"));
    assert!(transcript.contains("Report all affected files"));
    assert!(!transcript.contains("\"rawInput\""));
    assert_eq!(app.chat.permissions.len(), 1);
    presentation_tests::capture(&terminal, "permission-task-details");
    // Providers sometimes categorize delegation as execution. Its extra
    // fields must remain readable even when it is presented as a terminal.
    app.chat.session_mut().unwrap().apply(
        serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call_update", "toolCallId":"task-1", "kind":"execute"
        }))
        .unwrap(),
    );
    render(&mut app, &mut terminal);
    let transcript: String = app
        .chat
        .cache
        .rows
        .iter()
        .flat_map(|r| r.line.spans.iter().map(|s| s.content.as_ref()))
        .collect();
    assert!(transcript.contains("Subagent type: explore"));
    assert!(transcript.contains("Report all affected files"));
}

#[test]
fn permission_pattern_title_retains_search_operation_and_tool_identity() {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    *app.chat.session_mut().unwrap() = Session::new(id.clone());
    app.chat.session_mut().unwrap().apply(
        serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":"grep-1", "title":"grep", "kind":"search",
            "rawInput":{"pattern":"Thinking:", "path":"src"}
        }))
        .unwrap(),
    );
    app.chat.session_mut().unwrap().apply(serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"grep-2", "title":"another search", "kind":"search",
        "rawInput":{"pattern":"a different query"}
    })).unwrap());
    app.apply_acp_event(vtcode::acp::Event::Permission {
        ticket: 0,
        id: serde_json::from_value(serde_json::json!(22)).unwrap(),
        request: serde_json::from_value(serde_json::json!({"sessionId": id,
            "toolCall":{"toolCallId":"grep-1", "title":"Thinking:"},
            "options":[{"optionId":"once", "name":"Allow once", "kind":"allow_once"}]}))
        .unwrap(),
    });
    let (title, detail) = permissions::request_summary(&app);
    assert_eq!(title, "Search files · grep");
    assert!(detail.contains("Pattern: Thinking:"));
    assert!(detail.contains("Path: src"));
    assert!(!detail.contains("different query"));
    for width in [36, 90] {
        let mut terminal = Terminal::new(TestBackend::new(width, 38)).unwrap();
        render(&mut app, &mut terminal);
        assert!(text(&terminal).contains("Search files · grep"));
        presentation_tests::capture(&terminal, &format!("permission-search-{width}"));
        app.chat_action(ChatAction::Picker(Picker::Permissions));
        render(&mut app, &mut terminal);
        assert!(text(&terminal).contains("Search files · grep"));
        assert!(text(&terminal).contains("Pattern: Thinking:"));
        app.chat_action(ChatAction::DismissPicker);
    }
    assert_eq!(
        app.chat.permissions.len(),
        1,
        "inspecting details must not grant permission"
    );
}

#[test]
fn terminal_expand_and_collapse_share_one_header_and_identical_button_bounds() {
    for width in [36, 64, 110] {
        let mut app = tests::seeded();
        let id = app.chat.selected.clone().unwrap();
        *app.chat.session_mut().unwrap() = Session::new(id);
        app.chat.session_mut().unwrap().apply(serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":"test", "title":"Run tests", "kind":"execute", "status":"completed",
            "rawInput":{"command":"cargo test", "cwd":"src", "description":"Check the UI"},
            "rawOutput":{"stdout":"Done\n", "exitCode":0}
        })).unwrap());
        let mut terminal = Terminal::new(TestBackend::new(width, 44)).unwrap();
        render(&mut app, &mut terminal);
        let collapsed = app
            .chat
            .hits
            .iter()
            .find(|h| h.action == ChatAction::ToggleEntry(0))
            .unwrap()
            .area;
        app.chat_action(ChatAction::ToggleEntry(0));
        render(&mut app, &mut terminal);
        let expanded = app
            .chat
            .hits
            .iter()
            .find(|h| h.action == ChatAction::ToggleEntry(0))
            .unwrap()
            .area;
        assert_eq!(collapsed, expanded);
        let icon = (app.chat.transcript_area.unwrap().x, expanded.y);
        assert_eq!(terminal.backend().buffer()[icon].fg, app.colors.fg_success);
        assert!(
            app.chat.cache.rows[0].terminal.is_some(),
            "header must precede extra tool metadata"
        );
        presentation_tests::capture(&terminal, &format!("terminal-stable-header-{width}"));
    }
}

#[test]
fn minimal_reported_terminal_shares_header_status_and_controls_without_directory() {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    *app.chat.session_mut().unwrap() = Session::new(id);
    app.chat.output_detail = crate::app_acp::OutputDetail::Minimal;
    let tool: p::ToolCall = serde_json::from_value(serde_json::json!({
        "toolCallId":"history", "title":"Git history", "kind":"execute", "status":"completed",
        "rawInput":{"command":"git log --oneline -15", "cwd":"C:/workspace/hidden-directory"},
        "rawOutput":{"stdout":"Log output hidden until expanded"}
    }))
    .unwrap();
    app.chat
        .session_mut()
        .unwrap()
        .apply(p::SessionUpdate::ToolCall(tool));
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    render(&mut app, &mut terminal);
    assert_eq!(app.chat.cache.entry_height(0), 3);
    let screen = text(&terminal);
    assert!(screen.contains("Agent terminal · Completed"));
    assert!(screen.contains("git log --oneline -15"));
    assert!(!screen.contains("hidden-directory"));
    assert!(!screen.contains("Read-only"));
    assert!(!screen.contains("Log output"));
    let toggle = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == ChatAction::ToggleEntry(0))
        .unwrap();
    let header: String = (0..100)
        .map(|x| terminal.backend().buffer()[(x, toggle.area.y)].symbol())
        .collect();
    assert!(header.contains("Completed") && header.contains("Expand"));
    presentation_tests::capture(&terminal, "minimal-terminal");

    app.chat.session_mut().unwrap().busy = true;
    app.chat.session_mut().unwrap().apply(
        serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call_update", "toolCallId":"history", "status":"in_progress"
        }))
        .unwrap(),
    );
    for width in [24, 36, 100] {
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        render(&mut app, &mut terminal);
        assert!(app.chat.hits.iter().any(|h| h.action == ChatAction::Stop));
        assert!(
            app.chat
                .hits
                .iter()
                .any(|h| h.action == ChatAction::ToggleEntry(0))
        );
    }
}

#[test]
fn collapsed_terminal_commands_bound_wrapped_lines_and_expand_to_full_text() {
    let commands = [
        format!(
            "python -c @'\n{}\nCOMMAND_END\n'@",
            (0..20)
                .map(|i| format!("print('line {i}: 世界 🌍')"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        format!("python -c \"{}\"", "print('世界 🌍'); ".repeat(30)),
    ];
    for (case, command) in commands.iter().enumerate() {
        for width in [36, 80] {
            for detail in [OutputDetail::Minimal, OutputDetail::Compact] {
                for hosted in [false, true] {
                    let mut app = tests::seeded();
                    let id = app.chat.selected.clone().unwrap();
                    *app.chat.session_mut().unwrap() = Session::new(id.clone());
                    app.chat.output_detail = detail;
                    if hosted {
                        app.chat.session_mut().unwrap().terminal(
                            vtcode::acp::TerminalSnapshot {
                                session_id: id,
                                id: "long-command".into(),
                                command: command.clone(),
                                cwd: Default::default(),
                                output: "Done\n".into(),
                                truncated: false,
                                exit_status: Some(p::TerminalExitStatus::new().exit_code(0)),
                                released: true,
                                killed: false,
                                error: None,
                                history: None,
                                history_version: 0,
                            },
                        );
                    } else {
                        app.chat.session_mut().unwrap().apply(
                            serde_json::from_value(serde_json::json!({
                                "sessionUpdate":"tool_call", "toolCallId":"long-command",
                                "title":"Run script", "kind":"execute", "status":"completed",
                                "rawInput":{"command":command},
                                "rawOutput":{"stdout":"Done\n", "exitCode":0}
                            }))
                            .unwrap(),
                        );
                    }
                    let mut terminal = Terminal::new(TestBackend::new(width, 72)).unwrap();
                    render(&mut app, &mut terminal);
                    let collapsed_height = app.chat.cache.entry_height(0);
                    let limit = if detail == OutputDetail::Minimal {
                        6
                    } else {
                        10
                    };
                    assert!(
                        collapsed_height <= limit,
                        "collapsed command grew to {collapsed_height} rows at width {width}"
                    );
                    assert!(text(&terminal).contains("more lines"));
                    assert!(!text(&terminal).contains("COMMAND_END"));
                    presentation_tests::capture(
                        &terminal,
                        &format!("collapsed-command-{case}-{width}-{detail:?}-{hosted}"),
                    );

                    let more = app
                        .chat
                        .hits
                        .iter()
                        .rev()
                        .find(|hit| hit.action == ChatAction::ToggleEntry(0))
                        .unwrap()
                        .area;
                    let label: String = (more.x..more.right())
                        .map(|x| terminal.backend().buffer()[(x, more.y)].symbol())
                        .collect();
                    assert!(label.contains("more lines"));
                    assert!(app.handle_chat_mouse(MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: more.x,
                        row: more.y,
                        modifiers: KeyModifiers::empty(),
                    }));
                    render(&mut app, &mut terminal);
                    assert!(app.chat.cache.entry_height(0) > collapsed_height);
                    assert!(!text(&terminal).contains("more lines"));
                    if command.contains("COMMAND_END") {
                        assert!(text(&terminal).contains("COMMAND_END"));
                    }

                    app.chat_action(ChatAction::ToggleEntry(0));
                    render(&mut app, &mut terminal);
                    assert_eq!(app.chat.cache.entry_height(0), collapsed_height);
                    assert!(text(&terminal).contains("more lines"));
                }
            }
        }
    }
}

#[test]
fn terminal_expansion_scrolls_by_added_rows_and_long_commands_wrap() {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    *app.chat.session_mut().unwrap() = Session::new(id);
    for i in 0..15 {
        app.chat
            .session_mut()
            .unwrap()
            .push(ChatEntry::Agent(format!("Earlier message {i}")));
    }
    let command = "cargo test --offline --bin vtcode long_test_filter -- --nocapture --test-threads=1 final_argument";
    let tool: p::ToolCall = serde_json::from_value(serde_json::json!({
        "toolCallId":"test", "title":"Run tests", "kind":"execute", "status":"completed",
        "rawInput":{"command": command}, "rawOutput":{"stdout":"All tests passed\n", "exitCode":0}
    }))
    .unwrap();
    let index = app.chat.session().unwrap().entries.len();
    app.chat
        .session_mut()
        .unwrap()
        .apply(p::SessionUpdate::ToolCall(tool));
    let mut terminal = Terminal::new(TestBackend::new(54, 38)).unwrap();
    render(&mut app, &mut terminal);
    let before = app.chat.scroll;
    let old_height = app.chat.cache.entry_height(index);
    assert!(text(&terminal).contains("final"));
    assert!(text(&terminal).contains("_argument"));
    app.chat_action(ChatAction::ToggleEntry(index));
    render(&mut app, &mut terminal);
    let added = app.chat.cache.entry_height(index) - old_height;
    assert_eq!(added, 8); // The header stays on one row; only the output grows.
    assert_eq!(app.chat.scroll, before + added);
    assert!(text(&terminal).contains("_argument"));
    assert!(!app.chat.hits.iter().any(|h| matches!(
        h.action,
        ChatAction::TerminalTop(_)
            | ChatAction::FollowTerminal(_)
            | ChatAction::PanTerminal(..)
            | ChatAction::ScrollTerminal(..)
    )));
    presentation_tests::capture(&terminal, "wrapped-terminal-command");
}

#[test]
fn queue_preview_is_compact_editable_and_respects_narrow_bounds() {
    for (width, height) in [(100, 40), (54, 32), (28, 20)] {
        let mut app = tests::seeded();
        let id = app.chat.selected.clone().unwrap();
        app.chat.session_mut().unwrap().busy = true;
        app.chat.queues.insert(
            id.clone(),
            PromptQueue {
                items: (1..=5)
                    .map(|id| QueuedPrompt {
                        id,
                        text: format!("Next message {id}\nwith a second line"),
                        files: Vec::new(),
                        paste: None,
                    })
                    .collect(),
                ..Default::default()
            },
        );
        app.chat.input = "Prepare another message".into();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        render(&mut app, &mut terminal);
        assert!(text(&terminal).contains("Queue up"));
        assert!(text(&terminal).contains("Queued (5)"));
        assert!(
            app.chat
                .hits
                .iter()
                .all(|h| h.area.right() <= width && h.area.bottom() <= height)
        );
        app.chat_action(ChatAction::EditQueued(1));
        assert_eq!(app.chat.input, "Prepare another message");
        assert_eq!(app.chat.queues[&id].items.len(), 5);
        app.chat.input.clear();
        app.chat_action(ChatAction::EditQueued(1));
        assert_eq!(app.chat.input, "Next message 1\nwith a second line");
        assert_eq!(app.chat.queues[&id].items.len(), 4);
        render(&mut app, &mut terminal);
        presentation_tests::capture(&terminal, &format!("queue-{width}"));
        app.chat_action(ChatAction::PauseQueue);
        render(&mut app, &mut terminal);
        assert!(
            app.chat
                .hits
                .iter()
                .any(|hit| hit.action == ChatAction::QueuePage(false)),
            "Paused queues must retain paging controls at {width} columns"
        );
        for _ in 0..4 {
            app.chat_action(ChatAction::QueuePage(false));
            render(&mut app, &mut terminal);
        }
        assert!(
            app.chat
                .hits
                .iter()
                .any(|hit| hit.action == ChatAction::EditQueued(5)),
            "The last queued message must be reachable at {width} columns"
        );
    }
}

#[test]
fn message_boundaries_use_subtle_theme_rules() {
    let app = tests::seeded();
    let mut session = Session::new(p::SessionId::new("rules"));
    session.push(ChatEntry::User("A request".into()));
    session.push(ChatEntry::Agent("An answer".into()));
    for index in 0..2 {
        let rows = entry_rows(&session, index, 40, &app.colors, false, "Agent", None);
        let rules: Vec<_> = rows
            .iter()
            .filter(|r| r.line.spans.iter().any(|s| s.content.contains('─')))
            .collect();
        assert_eq!(rules.len(), 2);
        assert!(
            rules
                .iter()
                .all(|row| row.line.style.fg == Some(app.colors.ui.fg_group_border))
        );
    }
}
