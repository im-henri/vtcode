use super::*;

#[test]
fn delayed_permission_resolution_does_not_dismiss_a_new_generation_of_the_same_id() {
    let mut app = app();
    let id = p::RequestId::Number(41);
    app.receive_chat_permission(id.clone(), 1, request("first", 41), |_, _| true);
    app.apply_acp_event(Event::PermissionResolved(id.clone(), 1));
    app.receive_chat_permission(id.clone(), 2, request("first", 41), |_, _| true);
    app.apply_acp_event(Event::PermissionResolved(id.clone(), 1));
    assert_eq!(app.chat.permission().unwrap().ticket, 2);
    app.apply_acp_event(Event::PermissionResponded { id: id.clone(), ticket: 1, option: p::PermissionOptionId::new("temporary") });
    assert!(notices(&app, "first").is_empty());
    app.apply_acp_event(Event::PermissionResolved(id, 2));
    assert!(app.chat.permission().is_none());
}

fn app() -> App {
    let mut app = App::test_new();
    app.chat.initialized = Some(p::InitializeResponse::new(vtcode::acp::ProtocolVersion::V1));
    app.chat.visible = true;
    app.active_pane = ActivePane::Chat;
    add_session(&mut app, "first");
    app.chat.selected = Some(p::SessionId::new("first"));
    app
}

fn add_session(app: &mut App, id: &'static str) {
    let id = p::SessionId::new(id);
    let mut session = Session::new(id.clone());
    session.begin_prompt("A running turn".into());
    app.chat.sessions.insert(id.clone(), session);
    app.chat.open_tabs.push(id.clone());
    app.chat.session_order.push(id);
}

fn request(session: &str, id: i64) -> p::RequestPermissionRequest {
    serde_json::from_value(serde_json::json!({
        "sessionId": session,
        "toolCall": { "toolCallId": format!("tool-{id}"), "title": "Run checks", "kind": "execute" },
        "options": [
            { "optionId": "persistent", "name": "Allow once", "kind": "allow_always" },
            { "optionId": "deny", "name": "Reject", "kind": "reject_once" },
            { "optionId": "temporary", "name": "Permit", "kind": "allow_once" }
        ]
    })).unwrap()
}

fn receive(app: &mut App, session: &str, id: i64) -> Vec<Command> {
    let mut sent = Vec::new();
    app.receive_chat_permission(
        p::RequestId::Number(id), 0,
        request(session, id),
        |_, command| {
            sent.push(command);
            true
        },
    );
    sent
}

fn assert_once(commands: &[Command], expected: i64) {
    assert_eq!(commands.len(), 1);
    let Command::Permission { id, option, .. } = &commands[0] else {
        panic!("expected permission response");
    };
    assert_eq!(id, &p::RequestId::Number(expected));
    assert_eq!(option.as_ref().unwrap().to_string(), "temporary");
}

fn notices(app: &App, id: &str) -> Vec<String> {
    app.chat.sessions[&p::SessionId::new(id.to_owned())]
        .entries
        .iter()
        .filter_map(|entry| match entry {
            ChatEntry::Notice(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn confirm(app: &mut App, id: i64) {
    app.apply_acp_event(Event::PermissionResponded {
        ticket: 0,
        id: p::RequestId::Number(id),
        option: p::PermissionOptionId::new("temporary"),
    });
    app.apply_acp_event(Event::PermissionResolved(p::RequestId::Number(id), 0));
}

#[test]
fn yolo_checks_each_new_request_mid_turn_and_never_creates_a_persistent_rule() {
    let mut app = app();
    assert!(!app.chat.yolo_enabled());
    assert!(receive(&mut app, "first", 1).is_empty());
    let mut sent = Vec::new();
    app.toggle_chat_yolo(|_, command| {
        sent.push(command);
        true
    });
    assert_once(&sent, 1);
    assert!(app.chat.permissions.is_empty());
    assert_once(&receive(&mut app, "first", 2), 2);
    assert!(
        notices(&app, "first").is_empty(),
        "enqueueing is not yet an approval"
    );
    app.chat_action(ChatAction::Yolo);
    assert!(!app.chat.yolo_enabled());
    // Responses already queued while enabled still need accurate notices.
    confirm(&mut app, 1);
    confirm(&mut app, 2);
    assert_eq!(
        notices(&app, "first"),
        [
            "Accepted with YOLO · Run checks",
            "Accepted with YOLO · Run checks"
        ]
    );
    assert!(receive(&mut app, "first", 3).is_empty());
    assert_eq!(app.chat.permissions.len(), 1);
    assert!(app.chat.busy());
    sent.clear();
    app.toggle_chat_yolo(|_, command| {
        sent.push(command);
        true
    });
    assert_once(&sent, 3);
    assert_once(&receive(&mut app, "first", 4), 4);
}

#[test]
fn yolo_toggle_consumes_only_eligible_waiting_requests_for_that_conversation() {
    let mut app = app();
    add_session(&mut app, "second");
    let tool = p::ToolCall::new("tool-5", "grep");
    app.chat
        .session_mut()
        .unwrap()
        .push(ChatEntry::Tool(Box::new(tool)));
    let mut first = request("first", 5);
    first.tool_call.fields.title = Some("Thinking:".into());
    app.receive_chat_permission(p::RequestId::Number(5), 0, first, |_, _| {
        panic!("YOLO is still off")
    });
    assert!(receive(&mut app, "second", 6).is_empty());
    let mut missing_once = request("first", 7);
    missing_once
        .options
        .retain(|option| option.kind != p::PermissionOptionKind::AllowOnce);
    app.receive_chat_permission(p::RequestId::Number(7), 0, missing_once, |_, _| {
        panic!("YOLO is still off")
    });
    assert!(receive(&mut app, "first", 8).is_empty());
    app.toggle_chat_yolo(|_, command| {
        let Command::Permission { id, option, .. } = command else {
            panic!("expected permission response");
        };
        assert_eq!(option.unwrap().to_string(), "temporary");
        match id {
            p::RequestId::Number(5) => true,
            p::RequestId::Number(8) => false,
            _ => panic!("must preserve other conversations and missing Allow once"),
        }
    });
    assert_eq!(
        app.chat
            .permissions
            .iter()
            .map(|pending| pending.id.clone())
            .collect::<Vec<_>>(),
        [
            p::RequestId::Number(6),
            p::RequestId::Number(7),
            p::RequestId::Number(8)
        ]
    );
    confirm(&mut app, 5);
    assert!(
        notices(&app, "first")
            .iter()
            .any(|notice| notice == "Accepted with YOLO · grep")
    );
    assert!(notices(&app, "second").is_empty());
    assert_eq!(app.chat.permission().unwrap().id, p::RequestId::Number(7));
}
#[test]
fn yolo_background_requests_use_their_conversation_and_keep_the_current_draft() {
    let mut app = app();
    app.chat_action(ChatAction::Yolo);
    add_session(&mut app, "second");
    app.chat.select_tab(Some(p::SessionId::new("second")));
    app.chat.input = "Draft for another conversation".into();
    app.chat.focus = Some(ChatAction::ThinkingDetail);
    assert!(!app.chat.yolo_enabled());
    assert_once(&receive(&mut app, "first", 11), 11);
    confirm(&mut app, 11);
    assert_eq!(app.chat.focus, Some(ChatAction::ThinkingDetail));
    assert_eq!(app.chat.input, "Draft for another conversation");
    assert_eq!(notices(&app, "first"), ["Accepted with YOLO · Run checks"]);
    assert!(notices(&app, "second").is_empty());
    assert!(receive(&mut app, "second", 12).is_empty());
    assert_eq!(app.chat.permission().unwrap().id, p::RequestId::Number(12));
    app.chat.select_tab(Some(p::SessionId::new("first")));
    assert!(app.chat.yolo_enabled());
}

#[test]
fn yolo_agents_with_identical_session_and_request_ids_remain_independent() {
    let mut app = app();
    app.chat_action(ChatAction::Yolo);
    let mut second = AgentState::default();
    let id = p::SessionId::new("first");
    second.selected = Some(id.clone());
    second.sessions.insert(id.clone(), Session::new(id.clone()));
    app.chat.connections.insert(1, second);
    app.chat.agent_order.push(1);
    assert!(app.chat.switch_connection(1));
    assert!(!app.chat.yolo_enabled());
    assert!(receive(&mut app, "first", 21).is_empty());
    app.with_agent_context(0, true, |app| {
        assert_once(&receive(app, "first", 21), 21);
        confirm(app, 21);
    })
    .unwrap();
    assert_eq!(app.chat.active_agent, 1);
    assert!(notices(&app, "first").is_empty());
    assert_eq!(app.chat.permission().unwrap().id, p::RequestId::Number(21));
    app.apply_acp_event(Event::PermissionResolved(p::RequestId::Number(21), 0));
    assert!(app.chat.switch_connection(0));
    assert_eq!(notices(&app, "first"), ["Accepted with YOLO · Run checks"]);
    assert!(app.chat.yolo_enabled());
}

#[test]
fn yolo_without_allow_once_keeps_manual_choices_even_if_an_option_is_named_allow_once() {
    let mut app = app();
    app.chat_action(ChatAction::Yolo);
    let mut request = request("first", 31);
    request
        .options
        .retain(|option| option.kind != p::PermissionOptionKind::AllowOnce);
    app.receive_chat_permission(p::RequestId::Number(31), 0, request, |_, _| {
        panic!("must not select Allow always")
    });
    assert_eq!(app.chat.permission().unwrap().id, p::RequestId::Number(31));
    assert!(app.chat.yolo_approvals.is_empty());
    assert!(notices(&app, "first")[0].contains("did not offer Allow once"));
    assert!(!notices(&app, "first")[0].contains("Accepted with YOLO"));
}

#[test]
fn yolo_failed_sends_preserve_the_request_and_never_report_acceptance() {
    let mut app = app();
    app.chat_action(ChatAction::Yolo);
    app.receive_chat_permission(
        p::RequestId::Number(41), 0,
        request("first", 41),
        |_, command| {
            assert_once(&[command], 41);
            false
        },
    );
    assert_eq!(app.chat.permission().unwrap().id, p::RequestId::Number(41));
    assert!(app.chat.yolo_approvals.is_empty());
    confirm(&mut app, 41);
    assert!(notices(&app, "first").is_empty());

    // Exercise the real sender's error path without an active transport too.
    app.apply_acp_event(Event::Permission {
        ticket: 0,
        id: p::RequestId::Number(42),
        request: request("first", 42),
    });
    assert!(app.chat.error.is_some());
    assert_eq!(app.chat.permission().unwrap().id, p::RequestId::Number(42));
    assert!(app.chat.yolo_approvals.is_empty());
}

#[test]
fn yolo_cancelled_duplicate_and_mismatched_responses_do_not_log_false_acceptance() {
    let mut app = app();
    app.chat_action(ChatAction::Yolo);
    assert_once(&receive(&mut app, "first", 51), 51);
    assert!(receive(&mut app, "first", 51).is_empty());
    app.apply_acp_event(Event::PermissionResolved(p::RequestId::Number(51), 0));
    confirm(&mut app, 51);
    assert!(notices(&app, "first").is_empty());
    assert_once(&receive(&mut app, "first", 52), 52);
    app.apply_acp_event(Event::PermissionResponded {
        ticket: 0,
        id: p::RequestId::Number(52),
        option: p::PermissionOptionId::new("persistent"),
    });
    confirm(&mut app, 52);
    assert!(notices(&app, "first").is_empty());
    assert_once(&receive(&mut app, "first", 53), 53);
    confirm(&mut app, 53);
    confirm(&mut app, 53);
    assert_eq!(notices(&app, "first").len(), 1);
    assert!(app.chat.yolo_approvals.is_empty());
}

#[test]
fn yolo_notice_retains_the_tool_identity_when_the_permission_title_is_a_pattern() {
    let mut app = app();
    app.chat
        .session_mut()
        .unwrap()
        .push(ChatEntry::Tool(Box::new(p::ToolCall::new(
            "tool-61", "grep",
        ))));
    app.chat_action(ChatAction::Yolo);
    let mut request = request("first", 61);
    request.tool_call.fields.title = Some("Thinking:".into());
    app.receive_chat_permission(p::RequestId::Number(61), 0, request, |_, _| true);
    confirm(&mut app, 61);
    assert_eq!(notices(&app, "first"), ["Accepted with YOLO · grep"]);
}

#[test]
fn yolo_session_removal_and_disconnect_forget_policy_and_pending_approvals() {
    for closed in [false, true] {
        let mut app = app();
        app.chat_action(ChatAction::Yolo);
        assert_once(&receive(&mut app, "first", 71), 71);
        let id = p::SessionId::new("first");
        if closed {
            app.apply_acp_event(Event::SessionClosed(id.clone()));
        } else {
            app.chat.forget_conversation(&id);
        }
        assert!(app.chat.yolo_sessions.is_empty());
        assert!(app.chat.yolo_approvals.is_empty());
        add_session(&mut app, "first");
        app.chat.selected = Some(id);
        assert!(!app.chat.yolo_enabled());
        confirm(&mut app, 71);
        assert!(notices(&app, "first").is_empty());
    }
    for disconnected in [false, true] {
        let mut app = app();
        app.chat_action(ChatAction::Yolo);
        assert_once(&receive(&mut app, "first", 72), 72);
        if disconnected {
            app.apply_acp_event(Event::Disconnected("Gone".into()));
        } else {
            app.chat_action(ChatAction::Disconnect);
        }
        assert!(!app.chat.yolo_enabled());
        assert!(app.chat.yolo_approvals.is_empty());
        confirm(&mut app, 72);
        assert!(notices(&app, "first").is_empty());
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_yolo_toggle_restores_manual_approval() {
    use std::time::{Duration, Instant};
    fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            if done(app) {
                return;
            }
            assert!(Instant::now() < deadline, "Timed out: {}", app.chat.status);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    let mut config = AgentConfig::new(
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
    wait(&mut app, |app| app.chat.selected.is_some());
    let id = app.chat.selected.clone().unwrap().to_string();
    assert!(!app.chat.yolo_enabled());
    app.chat_action(ChatAction::Yolo);
    app.chat.input = "permission".into();
    app.chat_action(ChatAction::Send);
    wait(&mut app, |app| {
        !app.chat.busy()
            && notices(app, &id)
                .iter()
                .any(|notice| notice.contains("Accepted with YOLO"))
    });
    assert!(app.chat.permissions.is_empty());
    assert!(app.chat.yolo_approvals.is_empty());
    let accepted = notices(&app, &id);
    assert_eq!(accepted.len(), 1);
    assert!(accepted[0].contains("Update generated.txt"));

    app.chat_action(ChatAction::Yolo);
    app.chat.input = "permission".into();
    app.chat_action(ChatAction::Send);
    wait(&mut app, |app| app.chat.permission().is_some());
    assert!(app.chat.busy());
    assert_eq!(notices(&app, &id), accepted);
    let permission = app.chat.permission().unwrap();
    let option = permission
        .request
        .options
        .iter()
        .find(|option| option.kind == p::PermissionOptionKind::AllowOnce)
        .unwrap()
        .option_id
        .clone();
    let request = permission.id.clone();
    let ticket = permission.ticket;
    app.chat_action(ChatAction::Permission(request, ticket, Some(option)));
    wait(&mut app, |app| !app.chat.busy());
    assert!(app.chat.permissions.is_empty());
    assert_eq!(
        notices(&app, &id),
        accepted,
        "manual approvals must not claim YOLO"
    );

    // Turning on YOLO at a currently waiting card continues the same turn.
    app.chat.input = "permission".into();
    app.chat_action(ChatAction::Send);
    wait(&mut app, |app| app.chat.permission().is_some());
    app.chat_action(ChatAction::Yolo);
    wait(&mut app, |app| {
        !app.chat.busy() && notices(app, &id).len() == 2
    });
    assert!(app.chat.permissions.is_empty());
    assert!(app.chat.yolo_approvals.is_empty());
    assert!(notices(&app, &id)[1].contains("Accepted with YOLO"));
}
