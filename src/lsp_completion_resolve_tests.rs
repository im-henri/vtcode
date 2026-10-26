use super::*;
use crate::lsp::{LspEvent, TestLspServer};
use serde_json::json;

fn fixture() -> (LspManager, TestLspServer, tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.py");
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    let mut manager = LspManager::new(directory.path().to_path_buf());
    manager.clients.push(client);
    manager.sync_document_owned(&path, 1, "value".into());
    publish(
        &mut manager,
        &server,
        &path,
        json!([
            {"label":"value", "data":1}, {"label":"value", "data":2}
        ]),
    );
    (manager, server, directory, path)
}

fn publish(manager: &mut LspManager, server: &TestLspServer, path: &Path, items: Value) {
    assert!(manager.request_completion(path, 0, 5, "value"));
    server.respond(json!({"id":manager.active_completion_id,"result":items}));
    manager.poll();
    server.take_messages();
}

#[test]
fn completion_details_and_auto_imports_are_cached_when_revisiting_suggestions() {
    let (mut manager, server, _directory, path) = fixture();
    for index in [1, 0] {
        for _ in 0..32 {
            assert!(manager.request_completion_resolve(&path, index));
        }
        let messages = server.take_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["method"], "completionItem/resolve");
        assert_eq!(messages[0]["params"]["data"], index + 1);
        server.respond(json!({"id":manager.active_resolve_id,"result":{
            "label":"value", "data":index + 1, "detail":format!("overload {index}"),
            "documentation":{"kind":"markdown","value":format!("docs {index}")},
            "additionalTextEdits":[{"range":{"start":{"line":0,"character":0},
                "end":{"line":0,"character":0}},"newText":"import value\n"}]
        }}));
        manager.poll();
        assert_eq!(
            manager.completion_items[index].detail,
            Some(format!("overload {index}"))
        );
        assert_eq!(
            manager.completion_items[index].documentation,
            Some(format!("docs {index}"))
        );
        assert_eq!(
            manager.completion_items[index].additional_text_edits[0].new_text,
            "import value\n"
        );
        assert!(manager.active_resolve_id.is_none());
    }
    for _ in 0..100 {
        for index in [0, 1] {
            assert!(manager.request_completion_resolve(&path, index));
        }
    }
    assert!(
        server.take_messages().is_empty(),
        "revisiting resolved rows must not contact the server"
    );
    assert!(server.pending_requests.lock().unwrap().is_empty());
}

#[test]
fn identical_completion_items_resolve_the_selected_row() {
    let (mut manager, server, _directory, path) = fixture();
    publish(
        &mut manager,
        &server,
        &path,
        json!([{"label":"value"},{"label":"value"}]),
    );
    assert!(manager.request_completion_resolve(&path, 1));
    server.respond(json!({"id":manager.active_resolve_id,"result":{"label":"value","documentation":"selected"}}));
    manager.poll();
    assert!(manager.completion_items[0].documentation.is_none());
    assert_eq!(
        manager.completion_items[1].documentation.as_deref(),
        Some("selected")
    );
}

#[test]
fn moving_to_another_suggestion_cancels_work_and_ignores_buffered_replies() {
    let (mut manager, server, _directory, path) = fixture();
    manager.request_completion_resolve(&path, 0);
    let obsolete = manager.active_resolve_id.unwrap();
    manager.request_completion_resolve(&path, 1);
    let messages = server.take_messages();
    assert!(messages.iter().any(
        |message| message["method"] == "$/cancelRequest" && message["params"]["id"] == obsolete
    ));
    server.queue_event(LspEvent::CompletionResolveResponse {
        request_id: obsolete,
        item: Box::new(CompletionItem {
            label: "value".into(),
            documentation: Some(Documentation::String("obsolete".into())),
            ..Default::default()
        }),
    });
    manager.poll();
    assert!(
        manager
            .completion_items
            .iter()
            .all(|item| item.documentation.is_none())
    );
    // A cancelled row can be resolved again; it was never a successful cache hit.
    assert!(manager.request_completion_resolve(&path, 0));
    assert_ne!(manager.active_resolve_id, Some(obsolete));
    assert_eq!(server.pending_requests.lock().unwrap().len(), 1);
    manager.cancel_completion_request();
    assert!(server.pending_requests.lock().unwrap().is_empty());
}

#[test]
fn a_new_completion_list_cancels_old_details_and_resets_the_cache() {
    let (mut manager, server, _directory, path) = fixture();
    manager.request_completion_resolve(&path, 0);
    server.respond(
        json!({"id":manager.active_resolve_id,"result":{"label":"value","documentation":"old"}}),
    );
    manager.poll();
    manager.request_completion_resolve(&path, 1);
    let obsolete = manager.active_resolve_id.unwrap();
    assert!(manager.request_completion(&path, 0, 5, "value"));
    assert!(server.take_messages().iter().any(
        |message| message["method"] == "$/cancelRequest" && message["params"]["id"] == obsolete
    ));
    server
        .respond(json!({"id":manager.active_completion_id,"result":[{"label":"value","data":3}]}));
    server.queue_event(LspEvent::CompletionResolveResponse {
        request_id: obsolete,
        item: Box::new(CompletionItem {
            label: "value".into(),
            documentation: Some(Documentation::String("late".into())),
            ..Default::default()
        }),
    });
    manager.poll();
    assert!(manager.completion_items[0].documentation.is_none());
    assert!(manager.request_completion_resolve(&path, 0));
    assert_eq!(server.take_messages()[0]["params"]["data"], 3);
}

#[test]
fn unsupported_completion_details_do_not_inherit_another_servers_capability() {
    let (mut manager, first, directory, _path) = fixture();
    let path = directory.path().join("source.cpp");
    let (mut client, server) = LspClient::test_pair("cpp", &["*.cpp"]);
    client.install_capabilities(
        serde_json::from_value(json!({
            "completionProvider":{"resolveProvider":false}, "textDocumentSync":1
        }))
        .unwrap(),
    );
    manager.clients.push(client);
    manager.sync_document_owned(&path, 1, "value".into());
    publish(
        &mut manager,
        &server,
        &path,
        json!([{"label":"value","data":1}]),
    );
    for _ in 0..100 {
        assert!(manager.request_completion_resolve(&path, 0));
    }
    assert!(manager.completion_resolved_items.contains(&0));
    assert!(server.take_messages().is_empty());
    assert!(first.take_messages().is_empty());
    assert!(manager.active_resolve_id.is_none());
}

#[test]
fn failed_and_malformed_completion_details_finish_without_retrying_each_poll() {
    for response in [
        json!({"error":{"code":-32603,"message":"cannot resolve"}}),
        json!({"result":null}),
        json!({"result":{"detail":42}}),
    ] {
        let (mut manager, server, _directory, path) = fixture();
        manager.request_completion_resolve(&path, 0);
        let mut response = response;
        response["id"] = json!(manager.active_resolve_id);
        server.respond(response);
        manager.poll();
        assert!(manager.active_resolve_id.is_none());
        server.take_messages();
        for _ in 0..32 {
            assert!(manager.request_completion_resolve(&path, 0));
        }
        assert!(server.take_messages().is_empty());
        assert_eq!(manager.clients.len(), 1);
        assert!(!manager.clients[0].server_dead);
    }
}
