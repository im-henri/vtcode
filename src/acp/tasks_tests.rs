use super::*;
use crate::acp::{ChatEntry, Session};
use serde_json::json;

fn apply(session: &mut Session, update: Value) {
    session.apply(serde_json::from_value(update).unwrap());
}

fn tool(value: Value) -> p::ToolCall {
    serde_json::from_value(value).unwrap()
}

#[test]
fn tasks_cursor_update_todos_decode_enum_statuses_and_publish_on_success() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/acp-update-todos.json")).unwrap();
    let report = tool(fixture.clone());
    let tasks = tool_tasks(&report).unwrap();
    assert_eq!(tasks.entries.len(), 4);
    assert_eq!(tasks.current(), 0);
    assert_eq!(tasks.entries[0].status, TaskStatus::InProgress);
    assert_eq!(tasks.entries[3].status, TaskStatus::Pending);
    assert!(tasks.entries.iter().all(|entry| entry.priority.is_none()));

    let mut session = Session::new("cursor".into());
    session.apply(p::SessionUpdate::ToolCall(report));
    assert!(session.tasks.is_none());
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call_update",
        "toolCallId":"cursor-todos", "status":"completed"}),
    );
    assert_eq!(session.tasks.as_ref(), Some(&tasks));

    // A result snapshot is authoritative even when the input retains old states.
    let mut result = fixture["rawInput"].clone();
    result["todos"][0]["status"] = json!("TODO_STATUS_COMPLETED");
    result["todos"][1]["status"] = json!("TODO_STATUS_IN_PROGRESS");
    result["todos"][2]["status"] = json!("TODO_STATUS_CANCELLED");
    result["todos"][3]["status"] = json!("TODO_STATUS_CANCELED");
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call_update",
        "toolCallId":"cursor-todos", "rawOutput":{"content":[{"type":"text","text":result.to_string()}]}}),
    );
    let tasks = session.tasks.as_ref().unwrap();
    assert_eq!(tasks.completed(), 1);
    assert_eq!(tasks.current(), 1);
    assert_eq!(tasks.entries[2].status, TaskStatus::Cancelled);
    assert_eq!(tasks.entries[3].status, TaskStatus::Cancelled);
}

#[test]
fn tasks_reported_tool_names_and_summarized_titles_accept_output_only_lists() {
    let todos = json!([{"content":"Current work", "status":"TODO_STATUS_IN_PROGRESS"}]);
    for value in [
        json!({"toolCallId":"tasks", "title":"Update TODOs: Current work", "kind":"read", "rawOutput":todos}),
        json!({"toolCallId":"tasks", "title":"Current work", "kind":"read", "rawInput":{"tool_name":"updateTodos"}, "rawOutput":todos}),
        json!({"toolCallId":"tasks", "title":"Current work", "kind":"read", "rawOutput":{"toolName":"updateTodos","todos":todos}}),
    ] {
        assert_eq!(
            tool_tasks(&tool(value)).unwrap().entries[0].status,
            TaskStatus::InProgress
        );
    }
    for kind in ["edit", "execute", "search"] {
        assert!(
            tool_tasks(&tool(
                json!({"toolCallId":"file", "title":"Update TODOs: source code",
            "kind":kind, "rawOutput":{"tool_name":"updateTodos", "todos":todos}})
            ))
            .is_none()
        );
    }
    let tasks = tool_tasks(&tool(
        json!({"toolCallId":"tasks", "title":"updateTodos", "rawInput":{"todos":[
        {"content":"Earlier work", "status":"TODO_STATUS_IN_PROGRESS"},
        {"content":"Latest work", "status":"TODO_STATUS_IN_PROGRESS"}]}}),
    ))
    .unwrap();
    assert_eq!(tasks.current(), 1);
}

#[test]
fn tasks_decode_structured_todo_and_plan_tools_and_keep_cancelled_status() {
    let todos = json!([
        {"content":"Inspect code", "status":"completed", "priority":"high"},
        {"content":"Write fix", "status":"in_progress", "priority":"medium"},
        {"content":"Old approach", "status":"cancelled", "priority":"low"}
    ]);
    for value in [
        json!({"toolCallId":"tasks", "title":"todowrite", "rawInput":{"todos":todos}}),
        json!({"toolCallId":"tasks", "title":"3 todos", "rawInput":{"todos":todos}, "rawOutput":todos.to_string()}),
        json!({"toolCallId":"tasks", "title":"TodoWrite", "content":[{"type":"content","content":{"type":"text","text":todos.to_string()}}]}),
        json!({"toolCallId":"tasks", "title":"todoread", "kind":"read", "rawOutput":{"content":[{"type":"text","text":todos.to_string()}]}}),
    ] {
        let tasks = tool_tasks(&tool(value)).unwrap();
        assert_eq!(tasks.entries.len(), 3);
        assert_eq!(tasks.completed(), 1);
        assert_eq!(tasks.current(), 1);
        assert_eq!(tasks.entries[2].status, TaskStatus::Cancelled);
        assert_eq!(tasks.entries[0].priority, Some(p::PlanEntryPriority::High));
    }
    let tasks = tool_tasks(&tool(json!({"toolCallId":"plan", "title":"update_plan",
        "rawInput":{"plan":[{"step":"Run checks", "status":"pending"}]}})))
    .unwrap();
    assert_eq!(tasks.entries[0].content, "Run checks");
    assert_eq!(tasks.entries[0].priority, None);
    let tasks = tool_tasks(&tool(json!({"toolCallId":"vs", "title":"manage_todo_list",
        "rawInput":{"todoList":[{"id":1,"title":"Review change", "status":"in-progress"}]}})))
    .unwrap();
    assert_eq!(tasks.entries[0].status, TaskStatus::InProgress);
}

#[test]
fn tasks_do_not_misinterpret_source_files_commands_prose_or_partial_lists() {
    let todos = json!([{"content":"A task", "status":"pending"}]);
    for kind in ["read", "search", "edit", "execute"] {
        assert!(
            tool_tasks(&tool(
                json!({"toolCallId":"file", "title":"View todos.json", "kind":kind,
            "rawOutput":{"todos":todos}})
            ))
            .is_none()
        );
    }
    assert!(
        tool_tasks(&tool(
            json!({"toolCallId":"other", "title":"List records", "rawOutput":{"entries":[]}})
        ))
        .is_none()
    );
    for value in [
        json!({"todos":[{"content":"Missing status"}]}),
        json!({"todos":[{"content":"Valid", "status":"completed"},{"content":"Invalid", "status":"maybe"}]}),
        json!({"todos":[{"content":"", "status":"pending"}]}),
        json!({"todos":[{"content":"Unknown priority", "status":"pending", "priority":"urgent"}]}),
        json!({"todos":[{"content":"Invalid priority type", "status":"pending", "priority":1}]}),
        json!("TODO: inspect and test"),
    ] {
        assert!(
            tool_tasks(&tool(
                json!({"toolCallId":"bad", "title":"todowrite", "rawInput":value})
            ))
            .is_none()
        );
    }
}

#[test]
fn tasks_publish_successful_tools_replace_snapshots_and_ignore_late_status_updates() {
    let mut session = Session::new("tasks".into());
    let todos = json!([{"content":"Initial work", "status":"pending"}]);
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"old", "title":"todowrite",
        "status":"in_progress", "rawInput":{"todos":todos}}),
    );
    assert!(session.tasks.is_none());
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"old", "status":"completed"}),
    );
    assert_eq!(
        session.tasks.as_ref().unwrap().entries[0].content,
        "Initial work"
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"new", "title":"todowrite",
        "status":"completed", "rawInput":{"todos":[{"content":"New work", "status":"in_progress"}]}}),
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"old", "status":"completed"}),
    );
    assert_eq!(
        session.tasks.as_ref().unwrap().entries[0].content,
        "New work"
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"error", "title":"todowrite",
        "status":"completed", "rawInput":{"todos":[]}, "rawOutput":{"isError":true,"content":"Permission denied"}}),
    );
    assert_eq!(
        session.tasks.as_ref().unwrap().entries[0].content,
        "New work"
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"failed", "title":"todowrite",
        "status":"failed", "rawInput":{"todos":[{"content":"Rejected change", "status":"pending"}]}}),
    );
    assert_eq!(
        session.tasks.as_ref().unwrap().entries[0].content,
        "New work"
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"new",
        "rawOutput":{"todos":[{"content":"New work", "status":"completed"}]}}),
    );
    assert_eq!(session.tasks.as_ref().unwrap().completed(), 1);
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"empty", "title":"todowrite",
        "status":"completed", "rawInput":{"todos":[]}}),
    );
    assert!(session.tasks.as_ref().unwrap().entries.is_empty());
}

#[test]
fn tasks_native_plans_survive_new_prompts_cancellation_and_transcript_eviction() {
    let mut session = Session::new("tasks".into());
    apply(
        &mut session,
        json!({"sessionUpdate":"plan", "entries":[
        {"content":"Current task", "status":"in_progress", "priority":"high"}]}),
    );
    for _ in 0..1100 {
        session.push(ChatEntry::Notice("Activity".into()));
    }
    assert!(
        session
            .entries
            .iter()
            .all(|entry| !matches!(entry, ChatEntry::Plan(_)))
    );
    assert_eq!(
        session.tasks.as_ref().unwrap().entries[0].status,
        TaskStatus::InProgress
    );
    session.begin_prompt("Continue".into());
    session.finish(Some(p::StopReason::Cancelled));
    assert_eq!(
        session.tasks.as_ref().unwrap().entries[0].status,
        TaskStatus::InProgress
    );
    apply(&mut session, json!({"sessionUpdate":"plan", "entries":[]}));
    assert!(session.tasks.as_ref().unwrap().entries.is_empty());
}

#[test]
fn tasks_full_updates_to_an_earlier_tool_use_its_own_identity_and_session() {
    let mut session = Session::new("tasks".into());
    let other = Session::new("unrelated".into());
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"todo", "title":"todowrite",
        "status":"in_progress", "rawInput":{"todos":[{"content":"Earlier task", "status":"pending"}]}}),
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"later", "title":"read file", "kind":"read"}),
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"todo", "title":"todowrite",
        "status":"completed", "rawInput":{"todos":[{"content":"Earlier task", "status":"completed"}]}}),
    );
    assert_eq!(session.tasks.as_ref().unwrap().completed(), 1);
    assert!(other.tasks.is_none());
}

#[test]
fn tasks_late_completions_do_not_regress_newer_native_or_tool_snapshots() {
    let mut session = Session::new("tasks".into());
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call", "toolCallId":"old", "title":"todowrite",
        "status":"in_progress", "rawInput":{"todos":[{"content":"Do work", "status":"pending"}]}}),
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"plan", "entries":[
        {"content":"Do work", "status":"completed", "priority":"medium"}]}),
    );
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"old", "status":"completed"}),
    );
    assert_eq!(session.tasks.as_ref().unwrap().completed(), 1);
    // A new structured payload is still accepted, including on the same tool.
    apply(
        &mut session,
        json!({"sessionUpdate":"tool_call_update", "toolCallId":"old",
        "rawOutput":{"todos":[{"content":"Next task", "status":"in_progress"}]}}),
    );
    assert_eq!(
        session.tasks.as_ref().unwrap().entries[0].content,
        "Next task"
    );
    for id in ["slow", "fast"] {
        apply(
            &mut session,
            json!({"sessionUpdate":"tool_call", "toolCallId":id, "title":"todowrite",
            "status":"in_progress", "rawInput":{"todos":[{"content":id, "status":"pending"}]}}),
        );
    }
    for id in ["fast", "slow"] {
        apply(
            &mut session,
            json!({"sessionUpdate":"tool_call_update", "toolCallId":id, "status":"completed"}),
        );
    }
    assert_eq!(session.tasks.as_ref().unwrap().entries[0].content, "fast");
}
