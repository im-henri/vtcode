use super::*;
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
fn click(app: &mut App, terminal: &mut Terminal<TestBackend>, action: ChatAction) {
    render(app, terminal);
    let area = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == action)
        .unwrap_or_else(|| panic!("missing {action:?}"))
        .area;
    app.mouse_col = area.x;
    app.mouse_row = area.y;
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::empty(),
    });
    render(app, terminal);
}
pub(super) fn capture(terminal: &Terminal<TestBackend>, name: &str) {
    if let Some(directory) = std::env::var_os("VTCODE_CHAT_PRESENTATION_CAPTURE") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let buffer = terminal.backend().buffer();
        let cells: Vec<_> = buffer.content.iter().map(|c| serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg)})).collect();
        std::fs::write(directory.join(format!("presentation-{name}.json")), serde_json::to_vec(&serde_json::json!({"width":buffer.area.width,"height":buffer.area.height,"cells":cells})).unwrap()).unwrap();
    }
}

const CONTEXT_REPORT: &str = concat!(
    "Context Usage\n\n",
    "○ ○ ○ ◌ ◌ ◌ ● ◎ ◎ ◎    Auto → mai-code-1.1-flash · 28k/128k tokens (22%)\n",
    "◎ ◎ ◎ ◎ ◎ · · · · ·    ○ System Prompt          5.4k   (4%)\n",
    "· · · · · · · · · ·    ◌ System Tools           5.9k   (5%)\n",
    "· · · · · · · · · ·    ● MCP Tools              1.1k   (1%)\n",
    "· · · · · · · · · ·    ◎ Messages              15.4k  (12%)\n",
    "· · · · · · · · · ·    · Free Space            93.8k  (73%)\n",
    "· · · · · · ◎ ◎ ◎ ◎    ◎ Buffer                 6.4k   (5%)\n",
);

fn row_text(rows: &[ChatRow]) -> Vec<String> {
    rows.iter().map(|row| row.line.spans.iter().map(|span| span.content.as_ref()).collect()).collect()
}

#[test]
fn presentation_context_report_preserves_columns_and_adapts_to_narrow_panes() {
    let app = tests::seeded();
    let mut session = Session::new(p::SessionId::new("context"));
    session.push(ChatEntry::Agent(CONTEXT_REPORT.into()));
    for width in [120, 64, 36] {
        let rows = entry_rows(&session, 0, width, &app.colors, false, "Copilot", None);
        let lines = row_text(&rows);
        assert!(lines.iter().all(|line| line.width() <= width));
        for (label, count, percent) in [
            ("System Prompt", "5.4k", "(4%)"),
            ("System Tools", "5.9k", "(5%)"),
            ("MCP Tools", "1.1k", "(1%)"),
            ("Messages", "15.4k", "(12%)"),
            ("Free Space", "93.8k", "(73%)"),
            ("Buffer", "6.4k", "(5%)"),
        ] {
            let line = lines.iter().find(|line| line.contains(label)).unwrap();
            assert!(line.contains(count) && line.contains(percent), "{line}");
            assert_eq!(lines.iter().filter(|line| line.contains(label)).count(), 1);
        }
        if width == 120 {
            for original in CONTEXT_REPORT.lines().skip(2) {
                assert!(lines.iter().any(|line| line == original), "{original}");
            }
        } else {
            let grid_end = lines.iter().position(|line| line == "· · · · · · ◎ ◎ ◎ ◎").unwrap();
            let legend_start = lines.iter().position(|line| line.starts_with("Auto →")).unwrap();
            assert!(grid_end < legend_start);
            let count_ends: Vec<_> = lines.iter().filter_map(|line| {
                let percent = line.find('(')?;
                Some(line[..percent].trim_end().width())
            }).skip(1).collect();
            assert!(count_ends.windows(2).all(|pair| pair[0] == pair[1]));
        }
    }
    assert!(matches!(&session.entries[0], ChatEntry::Agent(text) if text == CONTEXT_REPORT));
    session.push(ChatEntry::Agent(CONTEXT_REPORT.replace("1.1k   (1%)", "56   (<1%)")));
    let rows = entry_rows(&session, 1, 36, &app.colors, false, "Copilot", None);
    let lines = row_text(&rows);
    let tools = lines.iter().find(|line| line.contains("MCP Tools")).unwrap();
    assert!(tools.contains("56") && tools.contains("(<1%)"));
}

#[test]
fn presentation_context_report_streams_replays_and_reflows_with_theme_and_resize() {
    let mut app = tests::seeded();
    let session = app.chat.session_mut().unwrap();
    *session = Session::new(session.id.clone());
    session.begin_prompt("/context".into());
    let mut terminal = Terminal::new(TestBackend::new(118, 38)).unwrap();
    // ACP chunk boundaries need not coincide with report lines or ANSI escapes.
    let report = format!("\x1b[90m{}\x1b[0m", CONTEXT_REPORT.replace('\n', "\r\n"));
    for chunk in report.char_indices().map(|(index, ch)| &report[index..index + ch.len_utf8()]) {
        app.chat.session_mut().unwrap().apply(p::SessionUpdate::AgentMessageChunk(
            p::ContentChunk::new(p::ContentBlock::Text(p::TextContent::new(chunk))),
        ));
        if chunk == "\n" {
            render(&mut app, &mut terminal);
        }
    }
    app.chat.session_mut().unwrap().finish(Some(p::StopReason::EndTurn));
    render(&mut app, &mut terminal);
    let streamed = text(&terminal);
    assert!(streamed.contains("○ ○ ○ ◌ ◌ ◌ ● ◎ ◎ ◎    Auto →"));
    let session = app.chat.session_mut().unwrap();
    *session = Session::new(session.id.clone());
    session.apply(p::SessionUpdate::UserMessageChunk(p::ContentChunk::new(
        p::ContentBlock::Text(p::TextContent::new("/context")),
    )));
    session.apply(p::SessionUpdate::AgentMessageChunk(p::ContentChunk::new(
        p::ContentBlock::Text(p::TextContent::new(report)),
    )));
    session.finish(Some(p::StopReason::EndTurn));
    render(&mut app, &mut terminal);
    assert_eq!(
        streamed.split_once("Context Usage").unwrap().1,
        text(&terminal).split_once("Context Usage").unwrap().1,
    );
    for (theme, width) in [("dark", 118), ("light", 74), ("monokai", 42)] {
        app.set_theme_by_key(theme);
        terminal.backend_mut().resize(width, 38);
        terminal.resize(Rect::new(0, 0, width, 38)).unwrap();
        render(&mut app, &mut terminal);
        let rendered = text(&terminal);
        for label in ["Context Usage", "System Prompt", "System Tools", "MCP Tools", "Messages", "Free Space", "Buffer"] {
            assert!(rendered.contains(label), "{theme}: {label}");
        }
        assert!(!rendered.contains("[90m"));
        capture(&terminal, &format!("context-{theme}-{width}"));
    }
}

#[test]
fn presentation_context_detection_does_not_replace_normal_markdown_or_fenced_reports() {
    let app = tests::seeded();
    for text in [
        "Context Usage\n\n**No diagram** here.",
        "An ordinary reply\n\n○ ○ ○    with a small diagram",
        &format!("```text\n{CONTEXT_REPORT}```"),
    ] {
        let mut rows = Vec::new();
        assert!(!context::render(&mut rows, text, 120, &app.colors));
        assert!(rows.is_empty());
    }
    let mut session = Session::new(p::SessionId::new("markdown"));
    session.push(ChatEntry::Agent("Context Usage\n\n**Still bold** and\nsoft-wrapped prose.".into()));
    let rows = entry_rows(&session, 0, 120, &app.colors, false, "Copilot", None);
    assert!(row_text(&rows).iter().any(|line| line == "Still bold and soft-wrapped prose."));
    assert!(rows.iter().flat_map(|row| &row.line.spans).any(|span|
        span.content.contains("Still bold") && span.style.add_modifier.contains(Modifier::BOLD)));
}

fn command_app() -> App {
    let mut app = tests::seeded();
    let session = app.chat.session_mut().unwrap();
    *session = Session::new(session.id.clone());
    session.begin_prompt("Can you run echo hello world in terminal".into());
    session.set_checkpoint(0, 9);
    session.apply(serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"cursor-shell", "title":"`echo hello world`", "kind":"execute", "status":"completed",
        "rawInput":{"command":"echo hello world"},
        "rawOutput":{"exitCode":0,"stdout":"\u{1b}[32mhello\r\nworld\u{1b}[0m\r\n","stderr":"Set-Location: directory diagnostic\r\nsecond diagnostic line\r\n"},
        "content":[{"type":"content","content":{"type":"text","text":"Not in allowlist: echo"}}]
    })).unwrap());
    session.finish(Some(p::StopReason::EndTurn));
    app
}

#[test]
fn presentation_agent_owned_commands_are_terminal_text_and_scroll_without_json() {
    let mut app = command_app();
    let mut terminal = Terminal::new(TestBackend::new(90, 44)).unwrap();
    render(&mut app, &mut terminal);
    let rendered = text(&terminal);
    for expected in [
        "Agent terminal",
        "$ echo hello world",
        "hello",
        "world",
        "Exit 0",
    ] {
        assert!(rendered.contains(expected), "{expected}");
    }
    for unwanted in [
        "Input:",
        "Output:",
        "exitCode",
        "stdout",
        "\\r\\n",
        "Restore files before this message",
    ] {
        assert!(!rendered.contains(unwanted), "{unwanted}");
    }
    assert!(app.chat.hits.iter().all(|h| !matches!(
        h.action,
        ChatAction::Terminal(_) | ChatAction::StopTerminal(_)
    )));
    click(&mut app, &mut terminal, ChatAction::ToggleEntry(1));
    assert!(text(&terminal).contains("[stderr]"));
    assert!(text(&terminal).contains("Set-Location: directory diagnostic"));
    assert_eq!(app.chat.terminal_areas[0].1.height, 10);
    for theme in ["dark", "light", "high-contrast"] {
        app.set_theme_by_key(theme);
        render(&mut app, &mut terminal);
        capture(&terminal, &format!("terminal-{theme}"));
    }
    let output = (0..150)
        .map(|i| format!("line-{i:03} {} END\n", "long output ".repeat(12)))
        .collect::<String>();
    app.chat.session_mut().unwrap().apply(serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call_update", "toolCallId":"cursor-shell", "rawOutput":{"exitCode":0,"stdout":output,"stderr":""},"content":[]
    })).unwrap());
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("line-000"));
    let outer_scroll = app.chat.scroll;
    let area = app.chat.terminal_areas[0].1;
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: area.x + 1,
        row: area.y,
        modifiers: KeyModifiers::empty(),
    });
    render(&mut app, &mut terminal);
    assert_eq!(app.chat.scroll, outer_scroll);
    assert!(text(&terminal).contains("line-003"));
    app.chat.focus = Some(ChatAction::FocusTerminal("tool:cursor-shell".into()));
    app.handle_chat_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::End, KeyModifiers::empty()));
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("line-149"));
    app.poll_acp();
    assert!(app.chat.terminal_views.contains_key("tool:cursor-shell"));
    app.chat.session_mut().unwrap().busy = true;
    app.chat.session_mut().unwrap().apply(serde_json::from_value(serde_json::json!({"sessionUpdate":"tool_call_update", "toolCallId":"cursor-shell", "status":"in_progress"})).unwrap());
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Stop response"));
    assert!(
        app.chat
            .hits
            .iter()
            .all(|h| !matches!(h.action, ChatAction::StopTerminal(_)))
    );
}

#[test]
fn presentation_command_decoder_preserves_other_tool_shapes() {
    let make = |kind: &str, result: serde_json::Value| -> p::ToolCall {
        serde_json::from_value(serde_json::json!({"toolCallId":"t","title":"Test", "kind":kind,"rawInput":{"command":"echo test"},"rawOutput":result})).unwrap()
    };
    assert!(
        tool_terminals::parse(
            &make("read", serde_json::json!({"stdout":"file contents"})),
            1
        )
        .is_none()
    );
    let unknown = tool_terminals::parse(
        &make("execute", serde_json::json!({"unknownPayload":"retain me"})), 1,
    ).unwrap();
    assert_eq!(unknown.lines, ["Unknown payload: retain me"]);
    let parsed =
        tool_terminals::parse(&make("execute", serde_json::json!("hello\r\nworld\r\n")), 1)
            .unwrap();
    assert_eq!(parsed.lines, ["hello", "world"]);
    let parsed = tool_terminals::parse(&make("execute", serde_json::json!("42")), 1).unwrap();
    assert_eq!(parsed.lines, ["42"], "numeric stdout is still text");
    let parsed = tool_terminals::parse(&make("execute", serde_json::json!(r#"{"answer":42}"#)), 1).unwrap();
    assert_eq!(parsed.lines, [r#"{"answer":42}"#], "JSON printed by the process is literal terminal output");
    let parsed = tool_terminals::parse(
        &make(
            "execute",
            serde_json::json!(r#"{"stdout":"hello\r\n","exitCode":0}"#),
        ),
        1,
    )
    .unwrap();
    assert_eq!(parsed.lines, ["hello"], "decode JSON string envelopes once");
    let parsed = tool_terminals::parse(
        &make(
            "other",
            serde_json::json!({"stdout":"hello\n","stderr":"error\n","exit_code":2}),
        ),
        2,
    )
    .unwrap();
    assert_eq!(parsed.lines, ["hello", "[stderr]", "error"]);
    let mut tool = make("execute", serde_json::json!({"stdout":"updated file"}));
    tool.content = serde_json::from_value(serde_json::json!([
        {"type":"diff","path":"example.rs","oldText":"before","newText":"after"}
    ]))
    .unwrap();
    assert!(
        tool_terminals::parse(&tool, 1).is_none(),
        "retain mixed diff content in the tool view"
    );
}

fn tool_app(tool: serde_json::Value) -> App {
    let mut app = tests::seeded();
    let session = app.chat.session_mut().unwrap();
    *session = Session::new(session.id.clone());
    session.apply(serde_json::from_value(tool).unwrap());
    app
}

#[test]
fn presentation_copilot_command_stays_a_terminal_after_completion_and_replay() {
    let mut app = tool_app(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"copilot-shell", "kind":"execute",
        "title":"Run focused Rust tests", "status":"in_progress",
        "rawInput":{"command":"cargo test -- --nocapture", "commands":["cargo test -- --nocapture"]}
    }));
    let mut terminal = Terminal::new(TestBackend::new(96, 38)).unwrap();
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Agent terminal"));
    click(&mut app, &mut terminal, ChatAction::ToggleEntry(0));
    let output = "   Compiling vtcode\n    Finished `test` profile\n     Running unittests src\\main.rs\n\nrunning 1 test\ntest sample ... ok\ntest result: ok. 1 passed\n<shellId: 0 completed with exit code 0>";
    let completion = serde_json::json!({
        "sessionUpdate":"tool_call_update", "toolCallId":"copilot-shell", "status":"completed",
        "rawOutput":{"content":output},
        "content":[{"type":"content","content":{"type":"text","text":output}}]
    });
    app.chat.session_mut().unwrap().apply(serde_json::from_value(completion).unwrap());
    render(&mut app, &mut terminal);
    let rendered = text(&terminal);
    for expected in ["Agent terminal", "$ cargo test -- --nocapture", "Finished `test` profile", "Exit 0", "Read-only"] {
        assert!(rendered.contains(expected), "missing {expected}");
    }
    for unwanted in ["Input:", "Output:", "shellId", "\\n", "content\":"] {
        assert!(!rendered.contains(unwanted), "unexpected {unwanted}");
    }
    assert_eq!(rendered.matches("test sample ... ok").count(), 1);
    assert_eq!(app.chat.terminal_areas[0].1.height, 10);
    assert!(app.chat.terminal_views.contains_key("tool:copilot-shell"));
    capture(&terminal, "copilot-terminal-completed");
    let ChatEntry::Tool(tool) = &app.chat.session().unwrap().entries[0] else { panic!("tool") };
    let replayed = tool.clone();
    let session = app.chat.session_mut().unwrap();
    *session = Session::new(p::SessionId::new("replayed"));
    session.apply(p::SessionUpdate::ToolCall(*replayed));
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Agent terminal"));
    assert!(text(&terminal).contains("Exit 0"));
    assert_eq!(app.chat.terminal_areas[0].1.height, 10);
}

#[test]
fn presentation_command_content_arrays_and_commandless_execute_keep_terminal_identity() {
    for raw in [
        serde_json::json!({"content":[{"type":"text","text":"first\nsecond\n"}]}),
        serde_json::json!({"result":{"content":"first\nsecond\n"}}),
    ] {
        let tool: p::ToolCall = serde_json::from_value(serde_json::json!({
            "toolCallId":"t", "kind":"execute", "title":"Run checks", "status":"completed", "rawOutput":raw
        })).unwrap();
        let parsed = tool_terminals::parse(&tool, 3).unwrap();
        assert_eq!(parsed.command, "Run checks");
        assert_eq!(parsed.lines, ["first", "second"]);
    }
}

#[test]
fn presentation_read_search_and_edit_tools_render_content_without_protocol_json() {
    let cases = [
        ("read", serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":"read", "kind":"read", "title":"Viewing src/main.rs", "status":"completed",
            "rawInput":{"path":"src/main.rs", "view_range":[1120,1205]},
            "rawOutput":{"content":"fn example() {\n    let message = \"hello\";\n    // source indentation\n}\n"}
        }), vec!["Lines 1120–1205", "fn example()", "let message", "source indentation"]),
        ("search", serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":"search", "kind":"search", "title":"Searching for example", "status":"completed",
            "rawInput":{"path":"C:\\work\\repo", "pattern":"example"},
            "rawOutput":{"content":"C:\\work\\repo\\src\\main.rs:335:    fn example() {}\nC:\\work\\repo\\docs\\guide.md:46:See example\n"},
            "locations":[{"path":"C:\\work\\repo"}]
        }), vec!["Search: example", "main.rs:335:", "guide.md:46:"]),
        ("edit", serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":"edit", "kind":"edit", "title":"Edit src/main.rs", "status":"completed",
            "rawInput":{"path":"src/main.rs", "old_str":"let old = 1;", "new_str":"let new = 2;"},
            "rawOutput":{"content":"Updated src/main.rs"},
            "content":[{"type":"diff","path":"src/main.rs","oldText":"let old = 1;\n","newText":"let new = 2;\n"}]
        }), vec!["Review", "let old = 1;", "let new = 2;", "Updated src/main.rs"]),
    ];
    for (name, tool, expected) in cases {
        let mut app = tool_app(tool);
        let mut terminal = Terminal::new(TestBackend::new(96, 42)).unwrap();
        click(&mut app, &mut terminal, ChatAction::ToggleEntry(0));
        for theme in ["dark", "light", "monokai"] {
            app.set_theme_by_key(theme);
            render(&mut app, &mut terminal);
            let rendered = text(&terminal);
            for expected in &expected { assert!(rendered.contains(expected), "{name}: missing {expected}"); }
            for unwanted in ["Input:", "Output:", "view_range", "old_str", "new_str", "\\n", "\\\""] {
                assert!(!rendered.contains(unwanted), "{name}: unexpected {unwanted}");
            }
            if name == "search" {
                assert!(app.chat.hits.iter().any(|h| h.action == ChatAction::OpenFile("C:\\work\\repo\\src\\main.rs".into(), Some(334))));
                assert!(app.chat.hits.iter().any(|h| h.action == ChatAction::OpenFile("C:\\work\\repo".into(), None)));
            }
            if name == "edit" {
                assert!(app.chat.hits.iter().any(|h| matches!(h.action, ChatAction::Review(_))));
            }
            capture(&terminal, &format!("tool-{name}-{theme}"));
        }
    }
}

#[test]
fn presentation_unknown_tool_fields_are_readable_and_source_fences_stay_literal() {
    let mut app = tool_app(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"unknown", "title":"Inspect result", "status":"completed",
        "rawOutput":{"elapsedMs":42,"details":{"messageCount":2,"available":true},"notes":["first line","second line"]}
    }));
    let mut terminal = Terminal::new(TestBackend::new(96, 42)).unwrap();
    click(&mut app, &mut terminal, ChatAction::ToggleEntry(0));
    for expected in ["Elapsed ms: 42", "Message count: 2", "Available: Yes", "first line", "second line"] {
        assert!(text(&terminal).contains(expected), "{expected}");
    }
    let mut app = tool_app(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"read", "kind":"read", "title":"Read Markdown", "status":"completed",
        "rawInput":{"path":"example.md"}, "rawOutput":{"content":"# heading\n```rust\nlet literal = 1;\n```\n"}
    }));
    click(&mut app, &mut terminal, ChatAction::ToggleEntry(0));
    assert!(text(&terminal).contains("# heading"));
    assert!(text(&terminal).contains("```rust"));
    let mut app = tool_app(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"read-json", "kind":"read", "title":"Read settings", "status":"completed",
        "rawInput":{"path":"settings.json"},
        "content":[{"type":"content","content":{"type":"text","text":"{\n  \"enabled\": true\n}"}}]
    }));
    click(&mut app, &mut terminal, ChatAction::ToggleEntry(0));
    assert!(text(&terminal).contains("\"enabled\": true"), "JSON source must remain source, including its quotes");
}

#[test]
fn presentation_collapsed_search_results_stay_bounded_after_wrapping() {
    let app = tool_app(serde_json::json!({
        "sessionUpdate":"tool_call", "toolCallId":"search", "kind":"search", "title":"Search results", "status":"completed",
        "rawOutput":{"content":format!("src/long.rs:1:{}\nsecond result\n", "wide output ".repeat(1000))}
    }));
    let rows = entry_rows(app.chat.session().unwrap(), 0, 36, &app.colors, false, "Agent", None);
    assert!(rows.len() <= 8, "collapsed tools must bound displayed rows after wrapping");
    assert!(rows.iter().any(|r| r.line.to_string() == "…"));
}

#[test]
fn presentation_thinking_effort_remains_visible_when_fixed_or_unreported() {
    let mut app = command_app();
    let mut terminal = Terminal::new(TestBackend::new(90, 40)).unwrap();
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Thinking: ?"));
    assert!(!text(&terminal).contains("Thinking: ? · Read-only"));
    click(
        &mut app,
        &mut terminal,
        ChatAction::Picker(Picker::ThinkingInfo),
    );
    assert!(text(&terminal).contains("cannot be changed here"));
    assert!(!text(&terminal).contains("free license"));
    capture(&terminal, "thinking-unreported");
    app.chat_action(ChatAction::DismissPicker);
    app.chat.session_mut().unwrap().config_options = serde_json::from_value(serde_json::json!([
        {"id":"effort","name":"Effort","category":"thought_level","type":"select","currentValue":"medium","options":[{"value":"medium","name":"Medium"}]}
    ])).unwrap();
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Thinking: Medium"));
    assert!(!text(&terminal).contains("Thinking: Medium · Read-only"));
    click(
        &mut app,
        &mut terminal,
        ChatAction::Picker(Picker::ThinkingInfo),
    );
    assert!(text(&terminal).contains("The agent reports Medium"));
    assert!(
        app.chat
            .choices
            .iter()
            .all(|c| !matches!(c.action, ChatAction::Config(_, _)))
    );
    capture(&terminal, "thinking-fixed");
    app.chat_action(ChatAction::DismissPicker);
    app.chat_action(ChatAction::Picker(Picker::Options));
    assert!(
        app.chat
            .choices
            .iter()
            .any(|c| c.action == ChatAction::Picker(Picker::ThinkingInfo)
                && c.detail == "Medium")
    );
    app.chat_action(ChatAction::Picker(Picker::Values("effort".into())));
    assert!(
        app.chat
            .choices
            .iter()
            .all(|c| c.action == ChatAction::DismissPicker)
    );
    app.chat_action(ChatAction::Config(
        "effort".into(),
        p::SessionConfigOptionValue::ValueId {
            value: "medium".into(),
        },
    ));
    assert!(
        app.chat.error.is_none(),
        "read-only choices cannot send configuration requests"
    );
    app.chat.session_mut().unwrap().config_options.splice(
        0..0,
        (0..5).map(|i| {
            p::SessionConfigOption::boolean(format!("extra-{i}"), format!("Extra {i}"), true)
        }),
    );
    let narrow = composer_selectors(&app, 42, 1);
    assert!(narrow.overflow);
    assert!(
        narrow
            .items
            .iter()
            .any(|(_, s)| s.picker == Picker::ThinkingInfo)
    );
    assert!(narrow.items.iter().all(|(r, _)| r.right() <= 38));
    app.chat_action(ChatAction::DismissPicker);
    app.chat.session_mut().unwrap().config_options = serde_json::from_value(serde_json::json!([
        {"id":"effort","name":"Effort","category":"thought_level","type":"select","currentValue":"medium","options":[]}
    ])).unwrap();
    assert_eq!(
        app.chat.thinking_status().0,
        "medium",
        "retain a reported value even without a display name"
    );
    app.chat.session_mut().unwrap().config_options = serde_json::from_value(serde_json::json!([
        {"id":"reasoning_effort","name":"Reasoning effort","type":"select","currentValue":"medium","options":[{"value":"low","name":"Low"},{"value":"medium","name":"Medium"}]}
    ])).unwrap();
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains("Thinking: Medium"));
    assert!(!text(&terminal).contains("Thinking: Medium · Read-only"));
    click(
        &mut app,
        &mut terminal,
        ChatAction::Picker(Picker::Values("reasoning_effort".into())),
    );
    assert!(
        app.chat
            .choices
            .iter()
            .any(|c| matches!(c.action, ChatAction::Config(_, _)))
    );
}

#[test]
fn presentation_user_message_hover_opens_an_anchored_actions_menu() {
    let mut app = command_app();
    let mut terminal = Terminal::new(TestBackend::new(90, 40)).unwrap();
    render(&mut app, &mut terminal);
    assert!(!text(&terminal).contains("Restore files before this message"));
    let action = ChatAction::Picker(Picker::Message(0, 0));
    let rows: Vec<_> = app
        .chat
        .hits
        .iter()
        .filter(|h| h.action == action)
        .map(|h| h.area)
        .collect();
    assert_eq!(rows.len(), 1, "one keyboard stop for the whole message");
    assert!(
        rows[0].height >= 2,
        "the message body must be clickable too"
    );
    app.mouse_col = rows.last().unwrap().x + 2;
    app.mouse_row = rows.last().unwrap().bottom() - 1;
    render(&mut app, &mut terminal);
    assert!(text(&terminal).contains(" ⋯ "));
    for area in &rows {
        let bg = terminal.backend().buffer()[(area.x, area.y)].bg;
        assert_ne!(bg, app.colors.ui.bg_chat_request);
    }
    capture(&terminal, "message-hover");
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: app.mouse_col,
        row: app.mouse_row,
        modifiers: KeyModifiers::empty(),
    });
    render(&mut app, &mut terminal);
    assert_eq!(app.chat.picker, Some(Picker::Message(0, 0)));
    assert!(text(&terminal).contains("Copy message"));
    assert!(text(&terminal).contains("Restore files before this message"));
    assert!(
        app.chat
            .choices
            .iter()
            .any(|c| c.action == ChatAction::Picker(Picker::Restore(9)))
    );
    assert!(
        app.chat
            .hits
            .iter()
            .filter(|h| matches!(h.action, ChatAction::Pick(_)))
            .all(|h| h.area.y > app.mouse_row)
    );
    capture(&terminal, "message-menu");
    app.chat_action(ChatAction::DismissPicker);
    app.chat.focus = Some(ChatAction::Picker(Picker::Message(0, 0)));
    render(&mut app, &mut terminal);
    app.handle_chat_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Tab,
        KeyModifiers::empty(),
    ));
    assert_ne!(
        app.chat.focus,
        Some(ChatAction::Picker(Picker::Message(0, 0)))
    );
    assert!(!app.chat.rollback.busy());
    assert!(app.chat.error.is_none());
}

#[test]
fn presentation_trimming_cannot_retarget_a_message_menu_to_another_checkpoint() {
    let mut app = command_app();
    let session = app.chat.session_mut().unwrap();
    while session.entries.len() < 1000 {
        let index = session.entries.len();
        session.push(ChatEntry::User(format!("Message {index}")));
        session.set_checkpoint(index, index as u64 + 100);
    }
    let action = ChatAction::Picker(Picker::Message(0, 0));
    app.chat_action(action.clone());
    assert!(
        app.chat
            .choices
            .iter()
            .any(|c| c.action == ChatAction::Picker(Picker::Restore(9)))
    );
    app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
        app.chat.selected.clone().unwrap(),
        p::SessionUpdate::AgentMessageChunk(p::ContentChunk::new(p::ContentBlock::Text(
            p::TextContent::new("More output"),
        ))),
    )));
    assert!(app.chat.session().unwrap().history_epoch > 0);
    assert!(app.chat.picker.is_none());
    assert!(app.chat.choices.is_empty());
    // A click from the previous rendered frame is equally harmless.
    app.chat_action(action);
    assert!(app.chat.choices.is_empty());
    assert!(!app.chat.rollback.busy());
}
