use super::*;
use serde_json::json;

#[test]
fn negotiated_incremental_sync_is_installed_before_deferred_open() {
    for sync in [json!(2), json!({"openClose":true,"change":2})] {
        let (mut manager, server, _temp, path) = fixture();
        manager.clients[0].initialized = false;
        let mut rope = Rope::from_str("# 😀\r\nvalue = 1\r\n");
        manager.sync_document(&path, 1, rope.clone());
        server.queue_event(LspEvent::Initialized { capabilities: Box::new(Some(serde_json::from_value(json!({"textDocumentSync":sync})).unwrap())) });
        manager.poll();
        let messages = server.take_messages();
        assert_eq!(messages[0]["method"], "initialized");
        assert_eq!(messages[1]["method"], "textDocument/didOpen");
        rope.insert(rope.line_to_char(1) + 8, "2");
        manager.sync_document(&path, 2, rope);
        let change = server.take_messages();
        assert_eq!(change[0]["params"]["contentChanges"][0]["text"], "2");
        assert_eq!(change[0]["params"]["contentChanges"][0]["range"]["start"], json!({"line":1,"character":8}));
    }
}

#[test]
fn polling_rotates_between_servers_and_wakes_for_remaining_work() {
    let (mut manager, first, temp, path) = fixture();
    let (client, second) = LspClient::test_pair("cpp", &["*.cpp"]);
    manager.clients.push(client);
    manager.sync_document_owned(&path, 1, "value = 1".into());
    let cpp = temp.path().join("source.cpp");
    manager.sync_document_owned(&cpp, 1, "int value;".into());
    manager.request_completion(&cpp, 0, 5, "v");
    for _ in 0..200 {
        first.queue_event(LspEvent::Diagnostics { uri: Url::from_file_path(&path).unwrap(), version: None, diagnostics: vec![] });
    }
    second.respond(json!({"id":manager.active_completion_id,"result":[{"label":"value"}]}));
    let generation = manager.wake.generation();
    // At most one first-server event precedes the other server's response.
    // A time-budget yield under load is fine; the next poll resumes there.
    for _ in 0..2 { manager.poll(); }
    assert_eq!(manager.completion_items[0].label, "value");
    assert!(manager.diagnostics_version < 200);
    assert!(manager.wake.generation() > generation);
    for _ in 0..200 { manager.poll(); }
    assert_eq!(manager.diagnostics_version, 1, "identical reports must not invalidate render caches");
}

#[test]
fn cancelling_dismissed_suggestions_releases_server_work_and_pending_ids() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "value(".into());
    manager.request_completion(&path, 0, 3, "val");
    manager.request_signature_help(&path, 0, 6);
    let ids = [manager.active_completion_id, manager.active_signature_id];
    server.take_messages();
    manager.cancel_completion_request();
    manager.cancel_signature_request();
    let messages = server.take_messages();
    for id in ids {
        assert!(messages.iter().any(|m| m["method"] == "$/cancelRequest" && m["params"]["id"].as_i64() == id));
    }
    assert!(server.pending_requests.lock().unwrap().is_empty());
    manager.cancel_completion_request();
    manager.cancel_signature_request();
    assert!(server.take_messages().is_empty());
    manager.request_completion(&path, 0, 3, "val");
    server.respond(json!({"id":manager.active_completion_id,"result":[{"label":"value"}]}));
    manager.poll();
    assert!(manager.request_completion_resolve(&path, 0));
    let id = manager.active_resolve_id.unwrap();
    server.take_messages();
    manager.cancel_completion_request();
    let messages = server.take_messages();
    assert!(messages.iter().any(|m| m["method"] == "$/cancelRequest" && m["params"]["id"] == id));
    assert!(server.pending_requests.lock().unwrap().is_empty());
}

#[test]
fn shared_deadline_yields_lsp_bursts_and_preserves_other_servers_responses() {
    let (mut manager, first, temp, path) = fixture();
    let (client, second) = LspClient::test_pair("cpp", &["*.cpp"]);
    manager.clients.push(client);
    manager.sync_document_owned(&path, 1, "value = 1".into());
    let cpp = temp.path().join("source.cpp");
    manager.sync_document_owned(&cpp, 1, "int value;".into());
    manager.request_completion(&cpp, 0, 5, "v");
    for _ in 0..200 {
        first.queue_event(LspEvent::Diagnostics { uri: Url::from_file_path(&path).unwrap(), version: None, diagnostics: vec![] });
    }
    second.respond(json!({"id":manager.active_completion_id,"result":[{"label":"value"}]}));
    let generation = manager.wake.generation();
    let expired = std::time::Instant::now();
    manager.poll_until(expired);
    assert!(manager.completion_items.is_empty());
    assert_eq!(manager.next_poll_client, 1);
    assert!(manager.wake.generation() > generation);
    manager.poll_until(expired);
    assert_eq!(manager.completion_items[0].label, "value");
    assert_eq!(manager.next_poll_client, 0);
}

#[test]
fn reader_prepares_completions_using_request_prefix_and_dispatch_adopts_the_same_storage() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "be".into());
    manager.request_completion(&path, 0, 2, "be");
    let id = manager.active_completion_id.unwrap();
    let event = LspClient::parse_message(&json!({"id":id,"result":[
        {"label":"alpha", "sortText":"1"},
        {"label":"beta", "sortText":"2", "documentation":"docs", "detail":"function",
         "insertTextFormat":2, "data":{"resolve":17}, "textEdit":{
            "newText":"beta(${1:value})", "insert":{"start":{"line":0,"character":0},"end":{"line":0,"character":2}},
            "replace":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}}},
         "additionalTextEdits":[{"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":0}},"newText":"import beta\n"}]}
    ]}).to_string(), &server.pending_requests).unwrap();
    let LspEvent::CompletionResponse { items, .. } = &event else { panic!("missing prepared completions"); };
    assert_eq!(items[0].label, "beta", "reader must use the prefix registered before send");
    assert_eq!(items[0].text_edit_range, Some(((0, 0), (0, 4))));
    assert_eq!(items[0].insert_text, "beta(${1:value})");
    assert!(items[0].is_snippet);
    assert_eq!(items[0].documentation.as_deref(), Some("docs"));
    assert_eq!(items[0].additional_text_edits.len(), 1);
    assert_eq!(items[0].original_item.as_ref().unwrap().data, Some(json!({"resolve":17})));
    let storage = items.as_ptr();
    server.queue_event(event);
    manager.poll();
    assert_eq!(manager.completion_items.as_ptr(), storage, "UI dispatch must not rebuild rows");
}

#[test]
fn already_prepared_cancelled_completion_cannot_replace_a_newer_list() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "value".into());
    manager.request_completion(&path, 0, 1, "a");
    let old = manager.active_completion_id.unwrap();
    server.respond(json!({"id":old,"result":[{"label":"alpha"}]}));
    manager.request_completion(&path, 0, 2, "be");
    let current = manager.active_completion_id.unwrap();
    server.respond(json!({"id":current,"result":[{"label":"alpha"},{"label":"beta"}]}));
    for _ in 0..2 { manager.poll(); }
    assert_eq!(manager.completion_items[0].label, "beta");
    manager.cancel_completion_request();
    manager.completion_items.clear();
    server.queue_event(LspEvent::CompletionResponse {
        request_id: current,
        items: completion::prepare_completions(vec![CompletionItem { label:"late".into(), ..Default::default() }], Some("")),
    });
    manager.poll();
    assert!(manager.completion_items.is_empty());
}

#[test]
fn invalid_completion_response_finishes_pending_ui_state_without_stopping_server() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "value".into());
    manager.request_completion(&path, 0, 2, "va");
    server.respond(json!({"id":manager.active_completion_id,"result":{"items":"invalid"}}));
    manager.poll();
    assert!(manager.active_completion_id.is_none());
    assert_eq!(manager.request_errors, vec!["textDocument/completion"]);
    assert!(!manager.clients[0].server_dead);
    assert!(server.pending_requests.lock().unwrap().is_empty());
}

fn fixture() -> (LspManager, TestLspServer, tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("source.py");
    let mut manager = LspManager::new(temp.path().to_path_buf());
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    manager.clients.push(client);
    (manager, server, temp, path)
}

#[test]
fn request_ids_are_unique_across_servers_and_restarts() {
    let (_manager, _server, temp, path) = fixture();
    // Retain receivers so the transport remains live.
    let (mut python, python_server) = LspClient::test_pair("python", &["*.py"]);
    let (mut clangd, clangd_server) = LspClient::test_pair("cpp", &["*.cpp"]);
    let first = python.request_hover(&path, 0, 0).unwrap();
    let second = clangd
        .request_hover(&temp.path().join("source.cpp"), 0, 0)
        .unwrap();
    assert_ne!(first, second);
    drop(python);
    let (mut restarted, _server) = LspClient::test_pair("python", &["*.py"]);
    assert_ne!(first, restarted.request_hover(&path, 0, 0).unwrap());
    assert_eq!(python_server.take_messages()[0]["id"], first);
    assert_eq!(clangd_server.take_messages()[0]["id"], second);
}

#[test]
fn configuration_requests_keep_order_sections_and_string_ids() {
    let (mut manager, server, _temp, _path) = fixture();
    manager.clients[0].settings = json!({"python":{"pythonPath":"C:/Python/python.exe", "analysis":{"typeCheckingMode":"strict"}}, "clangd.arguments":["--clang-tidy"]});
    for id in [json!(4), json!("configuration")] {
        server.respond(json!({"id":id,"method":"workspace/configuration","params":{"items":[
            {"section":"python.analysis"},{"section":"python"},{"section":"missing"},{},{"section":"clangd.arguments"}
        ]}}));
        manager.poll();
        let messages = server.take_messages();
        assert_eq!(messages[0]["id"], id);
        let result = &messages[0]["result"];
        assert_eq!(result[0]["typeCheckingMode"], "strict");
        assert_eq!(result[1]["pythonPath"], "C:/Python/python.exe");
        assert!(result[2].is_null());
        assert_eq!(result[3], manager.clients[0].settings);
        assert_eq!(result[4], json!(["--clang-tidy"]));
    }
}

#[test]
fn completion_ranking_honors_server_sort_and_filter_text() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "va".into());
    manager.request_completion(&path, 0, 2, "va");
    server.respond(json!({"id":manager.active_completion_id,"result":[
        {"label":"value_alphabetical","sortText":"020","filterText":"value"},
        {"label":"(int) value_preferred","sortText":"010","filterText":"value"}
    ]}));
    manager.poll();
    assert_eq!(manager.completion_items[0].label, "(int) value_preferred");
}

#[test]
fn completion_resolve_identifies_overloads_with_the_same_label() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "func".into());
    manager.request_completion(&path, 0, 4, "func");
    server.respond(json!({"id":manager.active_completion_id,"result":[
        {"label":"func","data":1},{"label":"func","data":2}
    ]}));
    manager.poll();
    manager.request_completion_resolve(&path, 1);
    server.respond(json!({"id":manager.active_resolve_id,"result":{"label":"func","data":2,"documentation":"second overload"}}));
    manager.poll();
    assert!(manager.completion_items[0].documentation.is_none());
    assert_eq!(
        manager.completion_items[1].documentation.as_deref(),
        Some("second overload")
    );
}

#[test]
fn null_navigation_and_hover_responses_finish_cleanly() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "pass".into());
    manager.request_definition(&path, 0, 0);
    server.respond(json!({"id":manager.active_definition_id,"result":null}));
    manager.request_references(&path, 0, 0);
    server.respond(json!({"id":manager.active_references_id,"result":null}));
    manager.request_hover(&path, 0, 0);
    server.respond(json!({"id":manager.active_hover_id,"result":null}));
    manager.poll();
    assert!(manager.definition_dirty && manager.definition_locations.is_empty());
    assert!(manager.references_dirty && manager.reference_locations.is_empty());
    assert!(manager.hover_content.is_none());
    assert!(manager.request_errors.is_empty());
}

#[test]
fn signature_parameter_offsets_are_converted_from_utf16_to_utf8_and_use_active_signature() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "call()".into());
    manager.request_signature_help(&path, 0, 5);
    let label = "call(é: str, 😀: int)";
    server.respond(json!({"id":manager.active_signature_id,"result":{"activeSignature":0,"activeParameter":0,
        "signatures":[{"label":label,"activeParameter":1,"parameters":[{"label":[5,11]},{"label":[13,20]}]}]}}));
    manager.poll();
    let help = manager.signature_help.as_ref().unwrap();
    assert_eq!(help.active_parameter, 1);
    let (start, end) = help.signatures[0].parameters[1];
    assert_eq!(&label[start..end], "😀: int");
}

#[test]
fn diagnostics_keep_codes_and_server_data_for_quick_fixes() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "bad".into());
    server.respond(json!({"method":"textDocument/publishDiagnostics","params":{"uri":Url::from_file_path(&path).unwrap(),"version":0,
        "diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":3}},"message":"bad token","code":"parse-error","data":{"fix":17}}]}}));
    manager.poll();
    let diagnostics = manager.code_action_diagnostics(&path, 0);
    assert_eq!(
        diagnostics[0].code,
        Some(NumberOrString::String("parse-error".into()))
    );
    assert_eq!(diagnostics[0].data, Some(json!({"fix":17})));
    manager.did_close(&path);
    assert!(manager.code_action_diagnostics(&path, 0).is_empty());
}

#[test]
fn semantic_token_legends_remain_specific_to_the_document_server() {
    let (mut manager, _server, temp, path) = fixture();
    let (mut clangd, _clangd_server) = LspClient::test_pair("cpp", &["*.cpp"]);
    let caps: ServerCapabilities = serde_json::from_value(json!({"semanticTokensProvider":{"legend":{"tokenTypes":["class","function"],"tokenModifiers":[]},"full":true}})).unwrap();
    clangd.install_capabilities(caps);
    manager.clients[0].install_capabilities(ServerCapabilities::default());
    manager.clients.push(clangd);
    assert!(manager.semantic_legend_for_file(Some(&path)).is_none());
    assert_eq!(
        manager
            .semantic_legend_for_file(Some(&temp.path().join("code.cpp")))
            .unwrap()
            .token_types[0],
        SemanticTokenType::CLASS
    );
}

#[test]
fn startup_coalesces_snapshots_and_sends_initialized_before_open() {
    let (mut manager, server, _temp, path) = fixture();
    manager.clients[0].initialized = false;
    for version in 1..=30 {
        manager.sync_document_owned(&path, version, format!("value = {version}\n"));
    }
    assert!(server.take_messages().is_empty());
    assert!(!manager.request_hover(&path, 0, 2));
    server.queue_event(LspEvent::Initialized {
        capabilities: Box::new(None),
    });
    manager.poll();
    let messages = server.take_messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["method"], "initialized");
    assert_eq!(messages[1]["method"], "textDocument/didOpen");
    assert_eq!(
        messages[1]["params"]["textDocument"]["text"],
        "value = 30\n"
    );
    assert_eq!(messages[1]["params"]["textDocument"]["version"], 29);
    assert_eq!(manager.doc_versions[&path], 29);
}

#[test]
fn closing_during_initialization_discards_the_deferred_open() {
    let (mut manager, server, _temp, path) = fixture();
    manager.clients[0].initialized = false;
    manager.sync_document_owned(&path, 1, "value = 1".into());
    manager.did_close(&path);
    manager.clients[0].send_initialized().unwrap();
    let messages = server.take_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["method"], "initialized");
}

#[test]
fn rapid_completion_cancels_obsolete_work_and_ignores_late_results() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "val".into());
    manager.request_completion(&path, 0, 1, "v");
    let old = manager.active_completion_id.unwrap();
    manager.request_completion(&path, 0, 3, "val");
    let current = manager.active_completion_id.unwrap();
    assert_ne!(old, current);
    let messages = server.take_messages();
    assert!(
        messages
            .iter()
            .any(|m| m["method"] == "$/cancelRequest" && m["params"]["id"] == old)
    );
    server.respond(json!({"id":old,"result":[{"label":"obsolete"}]}));
    server.respond(json!({"id":current,"result":[{"label":"value"}]}));
    manager.poll();
    assert_eq!(manager.completion_items.len(), 1);
    assert_eq!(manager.completion_items[0].label, "value");
}

#[test]
fn empty_completion_and_signature_results_clear_previous_popups() {
    let (mut manager, server, _temp, path) = fixture();
    manager.sync_document_owned(&path, 1, "call()".into());
    for result in [
        json!({"signatures":[{"label":"call(a: int)"}]}),
        Value::Null,
        json!({"signatures":[]}),
    ] {
        manager.request_signature_help(&path, 0, 5);
        server.respond(json!({"id":manager.active_signature_id.unwrap(),"result":result}));
        manager.poll();
        assert_eq!(
            manager.signature_help.is_some(),
            result
                .get("signatures")
                .and_then(Value::as_array)
                .is_some_and(|s| !s.is_empty())
        );
    }
    manager.request_completion(&path, 0, 2, "ca");
    let id = manager.active_completion_id.unwrap();
    let pending = server.pending_requests.clone();
    let event =
        LspClient::parse_message(&json!({"id":id,"result":null}).to_string(), &pending).unwrap();
    assert!(matches!(event, LspEvent::CompletionResponse { items, .. } if items.is_empty()));
}

#[test]
fn server_requests_preserve_string_ids_and_do_not_consume_client_requests() {
    let (mut manager, server, _temp, path) = fixture();
    let client_id = manager.clients[0].request_hover(&path, 0, 0).unwrap();
    server.take_messages();
    for id in [json!(client_id), json!("register-1")] {
        server.respond(
            json!({"id":id,"method":"client/registerCapability","params":{"registrations":[]}}),
        );
        manager.poll();
        let response = server.take_messages();
        assert_eq!(response.len(), 1);
        assert_eq!(response[0]["id"], id);
        assert!(response[0].get("result").is_some());
        assert!(
            server
                .pending_requests
                .lock()
                .unwrap()
                .contains_key(&client_id)
        );
    }
    server.respond(json!({"id":"unknown","method":"not/implemented"}));
    manager.poll();
    assert_eq!(server.take_messages()[0]["error"]["code"], -32601);
}

#[test]
fn unsupported_features_do_not_send_requests_or_kill_the_server() {
    let (mut manager, server, _temp, path) = fixture();
    manager.clients[0]
        .install_capabilities(serde_json::from_value(json!({"hoverProvider":true})).unwrap());
    manager.sync_document_owned(&path, 1, "value".into());
    server.take_messages();
    assert!(!manager.request_format_document(&path, 4, true));
    assert!(!manager.request_semantic_tokens_full(&path));
    assert!(!manager.request_completion(&path, 0, 3, "val"));
    assert!(server.take_messages().is_empty());
    assert!(manager.request_hover(&path, 0, 3));
    assert_eq!(server.take_messages()[0]["method"], "textDocument/hover");
    assert!(!manager.clients[0].server_dead);
}

#[test]
fn shared_clangd_process_uses_each_documents_language() {
    let temp = tempfile::tempdir().unwrap();
    let mut manager = LspManager::new(temp.path().to_path_buf());
    let (client, server) = LspClient::test_pair("cpp", &["*.c", "*.cpp", "*.h"]);
    manager.clients.push(client);
    for extension in ["c", "cpp", "h"] {
        manager.sync_document_owned(
            &temp.path().join(format!("source.{extension}")),
            1,
            "".into(),
        );
        assert_eq!(
            server.take_messages()[0]["params"]["textDocument"]["languageId"],
            if extension == "c" { "c" } else { "cpp" }
        );
    }
}

#[test]
fn framing_handles_utf8_byte_lengths_header_case_and_multiple_messages() {
    let body = json!({"message":"世界 🐍"}).to_string();
    let wire = format!(
        "content-length:{}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}Content-Length: 2\r\n\r\n{{}}",
        body.len(),
        body
    );
    let mut reader = std::io::Cursor::new(wire.as_bytes());
    assert_eq!(LspClient::read_message(&mut reader).unwrap(), Some(body));
    assert_eq!(
        LspClient::read_message(&mut reader).unwrap(),
        Some("{}".into())
    );
    assert_eq!(LspClient::read_message(&mut reader).unwrap(), None);
    assert!(
        LspClient::read_message(&mut std::io::Cursor::new(b"Content-Length: 10\r\n\r\n{}"))
            .is_err()
    );
}

#[test]
fn framing_bounds_headers_and_body_before_allocating_and_rejects_ambiguous_lengths() {
    for message in [
        "Content-Length: 67108865\r\n\r\n".to_owned(),
        "Content-Length: 18446744073709551615\r\n\r\n".to_owned(),
        "Content-Length: nope\r\n\r\n".to_owned(),
        "Content-Length: 2\r\nContent-Length: 3\r\n\r\n{}".to_owned(),
        "Content-Length: 2\r\n".to_owned(),
        "x".repeat(16*1024+1),
    ] {
        assert!(LspClient::read_message(&mut std::io::Cursor::new(message)).is_err());
    }
}
