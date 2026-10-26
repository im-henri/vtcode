use super::*;
use crate::app::Tab;
use crate::editor::Editor;
use crate::lsp::{LspClient, LspEvent, LspManager, LspServerConfig, TestLspServer};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn fixture() -> (App, TestLspServer, tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("first.rs");
    let content = "let value = 1;\n".repeat(300);
    std::fs::write(&path, &content).unwrap();
    let mut app = App::test_new();
    let mut editor = Editor::new();
    editor.open_file(&path).unwrap();
    editor.content_version = 1;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.hover_enabled = false;
    app.show_semantic_tokens = false;
    app.show_code_lenses = true;
    app.format_on_save = false;
    app.last_edit_time = Instant::now() - Duration::from_secs(2);
    app.lsp_manager = LspManager::new(directory.path().to_path_buf());
    app.explorer = crate::explorer::FileExplorer::new_deferred(directory.path().to_path_buf());
    app.lsp_manager.register_config(LspServerConfig {
        language: "fixture".into(),
        command: "unused-test-server".into(),
        args: Vec::new(),
        file_patterns: vec!["*.rs".into()],
        settings: None, init_options: None,
    });
    let (client, server) = LspClient::test_pair("fixture", &["*.rs"]);
    app.lsp_manager.clients.push(client);
    app.lsp_manager.sync_document_owned(&path, 1, content);
    assert_eq!(server.take_messages()[0]["method"], "textDocument/didOpen");
    (app, server, directory, path)
}

fn poll(app: &mut App) {
    poll_lsp(app, &mut false);
}

fn passive(messages: &[Value]) -> Vec<&Value> {
    messages
        .iter()
        .filter(|message| {
            matches!(
                message["method"].as_str(),
                Some(
                    "textDocument/inlayHint"
                        | "textDocument/documentSymbol"
                        | "textDocument/foldingRange"
                        | "textDocument/codeLens"
                        | "textDocument/documentLink"
                )
            )
        })
        .collect()
}

fn request_id(messages: &[Value], method: &str) -> i64 {
    messages
        .iter()
        .find(|message| message["method"] == method)
        .unwrap()["id"]
        .as_i64()
        .unwrap()
}

fn hint(label: &str) -> lsp_types::InlayHint {
    serde_json::from_value(json!({"position":{"line":0,"character":2}, "label":label})).unwrap()
}

#[test]
fn passive_features_wait_for_startup_then_request_the_original_snapshot() {
    let (mut app, server, _directory, path) = fixture();
    app.lsp_manager.clients[0].initialized = false;
    for _ in 0..32 { poll(&mut app); }
    assert!(server.take_messages().is_empty());
    assert!(app.lsp_manager.inlay_hint_request.is_none());
    assert!(app.lsp_manager.document_symbol_request.is_none());
    // The manager also protects callers outside the app scheduler.
    assert!(!app.lsp_manager.request_inlay_hints(&path, 0, 100));
    assert!(!app.lsp_manager.request_document_symbol(&path));
    assert!(app.lsp_manager.inlay_hint_request.is_none());
    assert!(app.lsp_manager.document_symbol_request.is_none());
    server.queue_event(LspEvent::Initialized { capabilities: Box::new(None) });
    poll(&mut app);
    let messages = server.take_messages();
    assert_eq!(passive(&messages).len(), 5);
    let id = request_id(&messages, "textDocument/inlayHint");
    server.respond(json!({"id":id,"result":[hint("ready")]}));
    poll(&mut app);
    assert_eq!(app.lsp_inlay_hints[0].label, "ready");
    assert_eq!(app.editor().unwrap().content_version, 1);
    for _ in 0..32 { poll(&mut app); }
    assert!(passive(&server.take_messages()).is_empty());
}

#[test]
fn unsupported_passive_features_are_recorded_without_building_requests_again() {
    let (mut app, server, _directory, _path) = fixture();
    app.lsp_manager.clients[0].install_capabilities(lsp_types::ServerCapabilities::default());
    app.lsp_inlay_hint_last_scroll = usize::MAX;
    poll(&mut app);
    assert_eq!(app.lsp_inlay_hint_last_scroll, 0);
    assert_eq!(app.lsp_outline_last_version, 1);
    assert_eq!(app.lsp_folding_last_version, 1);
    assert_eq!(app.lsp_code_lens_last_version, 1);
    assert_eq!(app.lsp_document_links_last_version, 1);
    assert!(app.lsp_manager.inlay_hint_request.is_some());
    assert!(app.lsp_manager.document_symbol_request.is_some());
    for _ in 0..32 { poll(&mut app); }
    assert!(passive(&server.take_messages()).is_empty());
}

#[test]
fn typing_coalesces_all_passive_features_after_the_exact_did_change() {
    let (mut app, server, _directory, path) = fixture();
    poll(&mut app);
    let first = server.take_messages();
    assert_eq!(passive(&first).len(), 5);
    let old_hint = request_id(&first, "textDocument/inlayHint");
    let old_symbol = request_id(&first, "textDocument/documentSymbol");
    app.lsp_inlay_hints = vec![crate::app::InlayHintDisplay::new(0, 2, "published", None)];

    for version in 2..=51 {
        let editor = app.editor_mut().unwrap();
        editor.rope.insert(0, "x");
        editor.content_version = version;
        editor.scroll_offset = version as usize;
        app.last_edit_time = Instant::now();
        app.lsp_inlay_hint_last_version = u64::MAX;
        app.lsp_manager.invalidate_document_feedback(&path);
        poll(&mut app);
    }
    let typing = server.take_messages();
    assert!(passive(&typing).is_empty());
    assert_eq!(
        typing
            .iter()
            .filter(|message| message["method"] == "$/cancelRequest")
            .count(),
        6 // Five passive features and the superseded document highlight.
    );
    assert_eq!(app.lsp_inlay_hints[0].label, "published");

    // Even a completed debounce is insufficient until the exact snapshot was
    // enqueued as didChange; the coalesced snapshot may still be pending.
    app.last_edit_time = Instant::now() - Duration::from_secs(2);
    poll(&mut app);
    assert!(passive(&server.take_messages()).is_empty());
    let latest_text = app.editor().unwrap().rope.to_string();
    app.lsp_manager.sync_document_owned(&path, 51, latest_text);
    poll(&mut app);
    let synchronized = server.take_messages();
    assert_eq!(synchronized[0]["method"], "textDocument/didChange");
    assert_eq!(passive(&synchronized).len(), 5);
    let requested_hint = passive(&synchronized)
        .into_iter()
        .find(|message| message["method"] == "textDocument/inlayHint")
        .unwrap();
    assert_eq!(requested_hint["params"]["range"]["start"]["line"], 1);
    assert_eq!(
        app.lsp_manager
            .inlay_hint_request
            .as_ref()
            .unwrap()
            .target
            .content_version,
        51
    );

    // Cancellation acknowledgements and replies buffered before cancellation
    // are both harmless and cannot replace the published decoration.
    server.respond(json!({"id":old_hint,"error":{"code":-32800,"message":"cancelled"}}));
    server.queue_event(LspEvent::InlayHintResponse {
        request_id: old_hint,
        hints: vec![hint("obsolete")],
    });
    server.queue_event(LspEvent::DocumentSymbolResponse {
        request_id: old_symbol,
        symbols: Vec::new(),
    });
    poll(&mut app);
    assert_eq!(app.lsp_inlay_hints[0].label, "published");
    assert_eq!(app.lsp_manager.clients.len(), 1);
    assert!(passive(&server.take_messages()).is_empty());
}

#[test]
fn completion_details_follow_arrow_keys_and_repaint_once_without_requerying_cached_rows() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut app, server, _directory, _path) = fixture();
    assert!(app.request_lsp_suggestions(false));
    server.respond(json!({"id":app.lsp_manager.active_completion_id,"result":[
        {"label":"first"}, {"label":"second"}
    ]}));
    poll(&mut app);
    let messages = server.take_messages();
    let id = request_id(&messages, "completionItem/resolve");
    server.respond(json!({"id":id,"result":{"label":"first","documentation":"first docs"}}));
    let mut redraw = false;
    poll_lsp(&mut app, &mut redraw);
    assert!(redraw, "new completion documentation must repaint without more input");
    assert!(!app.lsp_manager.completion_resolve_dirty);
    server.take_messages();
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    poll(&mut app);
    assert_eq!(app.lsp_completion_selected, 1);
    let messages = server.take_messages();
    let id = request_id(&messages, "completionItem/resolve");
    // Returning to a cached row cancels the other pending request.
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    poll(&mut app);
    let messages = server.take_messages();
    assert!(messages.iter().any(|message| message["method"] == "$/cancelRequest" && message["params"]["id"] == id));
    assert!(messages.iter().all(|message| message["method"] != "completionItem/resolve"));
    assert_eq!(app.lsp_manager.completion_items[0].documentation.as_deref(), Some("first docs"));
    for _ in 0..32 { poll(&mut app); }
    assert!(server.take_messages().is_empty());
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    poll(&mut app);
    let id = request_id(&server.take_messages(), "completionItem/resolve");
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    poll(&mut app);
    assert!(server.take_messages().iter().any(|message| message["method"] == "$/cancelRequest" && message["params"]["id"] == id));
    assert!(app.lsp_manager.active_resolve_id.is_none());
}

#[test]
fn completion_refresh_preserves_chosen_row_across_typing_backspace_and_resolve() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (mut app, server, _directory, path) = fixture();
    app.active_pane = crate::app::ActivePane::Editor;
    app.editor_mut().unwrap().set_cursor(0, 2); // "le"
    assert!(app.request_lsp_suggestions(false));
    let choices = json!([{"label":"letter"}, {"label":"length"}]);
    server.respond(json!({"id":app.lsp_manager.active_completion_id,"result":choices}));
    poll(&mut app);
    app.lsp_completion_selected = app.lsp_manager.completion_items.iter().position(|item| item.label == "letter").unwrap();
    poll(&mut app);
    let id = app.lsp_manager.active_resolve_id.unwrap();
    server.respond(json!({"id":id,"result":{"label":"letter","detail":"Resolved details"}}));
    poll(&mut app);
    for code in [KeyCode::Char('t'), KeyCode::Backspace] {
        crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code, KeyModifiers::NONE));
        assert_eq!(app.editor().unwrap().cursor_col, if code == KeyCode::Backspace { 2 } else { 3 });
        assert!(app.lsp_completion_active);
        // Complete the asynchronous document handoff before delivering a response.
        let editor = app.editor().unwrap();
        let version = editor.content_version;
        let text = editor.rope.to_string();
        app.lsp_manager.sync_document_owned(&path, version, text);
        app.dispatch_lsp_suggestions();
        server.respond(json!({"id":app.lsp_manager.active_completion_id,"result":choices}));
        let mut redraw = false;
        poll_lsp(&mut app, &mut redraw);
        assert!(redraw, "new list must repaint even if it has the same number of rows");
        assert_eq!(app.lsp_manager.completion_items[app.lsp_completion_selected].label, "letter");
    }
}

#[test]
fn completion_explicit_empty_results_feedback_and_preselection_are_applied_once() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let (mut app, server, _directory, _path) = fixture();
    crate::input::handle_editor_key(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(app.completion_ui.explicit);
    server.respond(json!({"id":app.lsp_manager.active_completion_id,"result":[]}));
    poll(&mut app);
    assert_eq!(app.completion_ui.status, Some("No suggestions"));
    assert!(app.lsp_completion_active);
    assert!(app.request_lsp_suggestions(false));
    server.respond(json!({"id":app.lsp_manager.active_completion_id,"result":[
        {"label":"alpha"}, {"label":"zebra","preselect":true}
    ]}));
    poll(&mut app);
    assert_eq!(app.lsp_manager.completion_items[app.lsp_completion_selected].label, "zebra");
    app.lsp_completion_selected = 0;
    poll(&mut app);
    assert_eq!(app.lsp_completion_selected, 0, "polling must not overwrite the user's selection");
}

#[test]
fn completion_accept_rejects_a_changed_document_even_before_the_next_poll() {
    let (mut app, server, _directory, _path) = fixture();
    assert!(app.request_lsp_suggestions(false));
    server.respond(json!({"id":app.lsp_manager.active_completion_id,"result":[{"label":"inserted"}]}));
    poll(&mut app);
    app.editor_mut().unwrap().insert_char_replacing_selection('x');
    let changed = app.editor().unwrap().rope.clone();
    app.apply_lsp_completion();
    assert_eq!(app.editor().unwrap().rope, changed);
    assert!(!app.lsp_completion_active);
}

#[test]
fn synchronized_scroll_reuses_covered_rows_and_requests_new_ranges_immediately() {
    let (mut app, server, _directory, _path) = fixture();
    poll(&mut app);
    let initial = server.take_messages();
    app.lsp_inlay_hints = vec![crate::app::InlayHintDisplay::new(0, 2, "old hint", None)];
    app.lsp_outline_available = true;
    for request in passive(&initial) {
        let result = if matches!(
            request["method"].as_str(),
            Some("textDocument/inlayHint" | "textDocument/documentSymbol")
        ) {
            Value::Null
        } else {
            json!([])
        };
        server.respond(json!({"id":request["id"],"result":result}));
    }
    poll(&mut app);
    assert!(app.lsp_inlay_hints.is_empty());
    assert!(!app.lsp_outline_available);
    assert!(app.lsp_manager.active_inlay_hint_id.is_none());
    assert!(app.lsp_manager.active_document_symbol_id.is_none());
    server.take_messages();
    app.last_edit_time = Instant::now();
    for offset in 1..=50 {
        app.editor_mut().unwrap().scroll_offset = offset;
        poll(&mut app);
    }
    assert!(passive(&server.take_messages()).is_empty(), "small scrolls reuse the prefetched range");
    app.editor_mut().unwrap().scroll_offset = 51;
    poll(&mut app);
    let scroll = server.take_messages();
    assert_eq!(passive(&scroll).len(), 1);
    assert_eq!(passive(&scroll)[0]["method"], "textDocument/inlayHint");
    let old_id = request_id(&scroll, "textDocument/inlayHint");
    for _ in 0..32 {
        poll(&mut app);
    }
    assert!(passive(&server.take_messages()).is_empty());

    for offset in 52..=101 {
        app.editor_mut().unwrap().scroll_offset = offset;
        poll(&mut app);
    }
    assert!(server.take_messages().is_empty(), "keep the pending request while scrolling within it");
    app.editor_mut().unwrap().scroll_offset = 102;
    poll(&mut app);
    let next = server.take_messages();
    assert_eq!(next[0]["method"], "$/cancelRequest");
    assert_eq!(next[0]["params"]["id"], old_id);
    assert_eq!(passive(&next).len(), 1);
    let latest = request_id(&next, "textDocument/inlayHint");
    server.respond(json!({"id":latest,"result":[]}));
    poll(&mut app);
    app.last_edit_time = Instant::now() - Duration::from_secs(2);
    for _ in 0..32 {
        poll(&mut app);
    }
    assert!(passive(&server.take_messages()).is_empty());
}

#[test]
fn request_errors_retire_work_without_killing_the_server_or_repeating_unsupported_features() {
    let (mut app, server, _directory, path) = fixture();
    poll(&mut app);
    let initial = server.take_messages();
    for request in passive(&initial) {
        server
            .respond(json!({"id":request["id"],"error":{"code":-32601,"message":"not supported"}}));
    }
    assert!(app.lsp_manager.request_format_document(&path, 4, true));
    app.lsp_format_pending = true;
    app.lsp_format_target_path = Some(path.clone());
    let format = server.take_messages();
    server.respond(json!({"id":request_id(&format,"textDocument/formatting"),
        "error":{"code":-32800,"message":"cancelled"}}));
    poll(&mut app);
    assert!(!app.lsp_format_pending);
    assert!(app.lsp_format_target_path.is_none());
    assert!(!app.lsp_outline_available);
    assert_eq!(app.lsp_manager.clients.len(), 1);
    assert!(!app.lsp_manager.clients[0].server_dead);
    for _ in 0..32 {
        poll(&mut app);
    }
    assert!(passive(&server.take_messages()).is_empty());

    app.editor_mut().unwrap().content_version = 2;
    app.lsp_manager
        .sync_document_owned(&path, 2, "updated\n".into());
    poll(&mut app);
    let later = server.take_messages();
    assert_eq!(later[0]["method"], "textDocument/didChange");
    assert_eq!(passive(&later).len(), 5);
}

#[test]
fn equal_versions_on_different_servers_and_close_cannot_accept_obsolete_replies() {
    let (mut app, first_server, directory, _path) = fixture();
    poll(&mut app);
    let initial = first_server.take_messages();
    let first_hint = request_id(&initial, "textDocument/inlayHint");
    let first_symbol = request_id(&initial, "textDocument/documentSymbol");
    let second_path = directory.path().join("second.py");
    let mut editor = Editor::test_with_text("second\n");
    editor.file_path = Some(second_path.clone());
    editor.content_version = 1;
    app.tabs.push(Tab::new_file(editor));
    let (second_client, second_server) = LspClient::test_pair("second", &["*.py"]);
    app.lsp_manager.clients.push(second_client);
    app.lsp_manager
        .sync_document_owned(&second_path, 1, "second\n".into());
    second_server.take_messages();
    app.active_tab = 1;
    poll(&mut app);
    let second = second_server.take_messages();
    assert_eq!(passive(&second).len(), 5);
    assert_ne!(request_id(&second, "textDocument/inlayHint"), first_hint);
    assert_ne!(
        request_id(&second, "textDocument/documentSymbol"),
        first_symbol
    );
    first_server.queue_event(LspEvent::InlayHintResponse {
        request_id: first_hint,
        hints: vec![hint("wrong server")],
    });
    first_server.queue_event(LspEvent::DocumentSymbolResponse {
        request_id: first_symbol,
        symbols: Vec::new(),
    });
    poll(&mut app);
    assert!(app.lsp_inlay_hints.is_empty());
    assert!(!app.lsp_outline_available);

    let second_hint = request_id(&second, "textDocument/inlayHint");
    app.lsp_manager.did_close(&second_path);
    app.tabs.clear();
    second_server.queue_event(LspEvent::InlayHintResponse {
        request_id: second_hint,
        hints: vec![hint("closed file")],
    });
    poll(&mut app);
    assert!(app.lsp_inlay_hints.is_empty());
    assert!(passive(&second_server.take_messages()).is_empty());
    assert!(app.lsp_manager.inlay_hint_request.is_none());
    assert!(app.lsp_manager.document_symbol_request.is_none());
}

#[test]
fn save_orders_did_change_before_did_save_and_refreshes_only_the_latest_snapshot() {
    let (mut app, server, _directory, path) = fixture();
    poll(&mut app);
    server.take_messages();
    let editor = app.editor_mut().unwrap();
    editor.rope.insert(0, "saved ");
    editor.content_version = 2;
    editor.modified = true;
    app.last_edit_time = Instant::now();
    app.lsp_manager.invalidate_document_feedback(&path);
    app.save_current_file();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut messages = Vec::new();
    while Instant::now() < deadline {
        app.poll_background_results();
        poll(&mut app);
        messages.extend(server.take_messages());
        if messages
            .iter()
            .any(|message| message["method"] == "textDocument/didSave")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let change = messages
        .iter()
        .position(|message| message["method"] == "textDocument/didChange")
        .unwrap();
    let save = messages
        .iter()
        .position(|message| message["method"] == "textDocument/didSave")
        .unwrap();
    assert!(change < save);
    assert!(passive(&messages).is_empty());
    app.last_edit_time = Instant::now() - Duration::from_secs(2);
    poll(&mut app);
    assert_eq!(passive(&server.take_messages()).len(), 5);
    assert_eq!(
        app.lsp_manager
            .document_symbol_request
            .as_ref()
            .unwrap()
            .target
            .content_version,
        2
    );
}

#[test]
fn editor_navigation_requests_use_utf16_and_results_restore_scalar_columns() {
    let (mut app, server, _directory, path) = fixture();
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str("/*😀*/ int target;\n");
    editor.set_cursor(0, 12);
    app.execute_palette_command("go_to_definition");
    let messages = server.take_messages();
    let request = messages.iter().find(|m| m["method"] == "textDocument/definition").unwrap();
    assert_eq!(request["params"]["position"]["character"], 13);
    server.respond(json!({"id":request["id"],"result":{"uri":lsp_types::Url::from_file_path(&path).unwrap(),
        "range":{"start":{"line":0,"character":11},"end":{"line":0,"character":17}}}}));
    poll(&mut app);
    assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (0, 10));
}

#[test]
fn unicode_navigation_to_unopened_file_waits_for_the_real_text() {
    let (mut app, _server, directory, _path) = fixture();
    let target = directory.path().join("target.rs");
    std::fs::write(&target, "// first\n/*😀*/ int target;\n").unwrap();
    app.open_file(&target);
    app.editor_mut().unwrap().set_lsp_cursor(1, 11);
    assert!(app.editor().unwrap().bg_loading);
    app.wait_for_background(Duration::from_secs(5), |app| !app.editor().unwrap().bg_loading);
    assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (1, 10));
}

#[test]
fn stale_formatting_does_not_overwrite_typing() {
    let (mut app, server, _directory, path) = fixture();
    assert!(app.lsp_manager.request_format_document(&path, 4, true));
    app.lsp_format_pending = true;
    app.lsp_format_target_path = Some(path);
    let id = app.lsp_manager.active_format_id.unwrap();
    let editor = app.editor_mut().unwrap();
    editor.save_undo_public();
    editor.rope.insert(0, "// typing\n");
    server.respond(json!({"id":id,"result":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":14}},"newText":"OLD FORMAT"}]}));
    poll(&mut app);
    assert!(app.editor().unwrap().rope.to_string().starts_with("// typing\n"));
    assert!(!app.lsp_format_pending);
    assert!(!app.editor().unwrap().rope.to_string().contains("OLD FORMAT"));
}

#[test]
fn synchronized_typing_does_not_expose_transient_problems_in_editor_decorations() {
    let (mut app, server, _directory, path) = fixture();
    app.lsp_manager.diagnostic_publication.set_delay_ms(10_000);
    crate::ui_editor::refresh_cached_editor_diagnostics(&mut app);
    assert!(app.cached_editor_diagnostics.is_empty());
    let before = std::sync::Arc::clone(&app.cached_editor_diagnostics);
    crate::input::handle_editor_key(&mut app, crossterm::event::KeyCode::Char('x'), crossterm::event::KeyModifiers::NONE);
    let version = app.editor().unwrap().content_version;
    app.wait_for_background(Duration::from_secs(5), |app| app.lsp_manager.last_notified_version.get(&path) == Some(&version));
    poll(&mut app);
    server.respond(json!({"method":"textDocument/publishDiagnostics","params":{
        "uri":lsp_types::Url::from_file_path(&path).unwrap(),
        "diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
            "severity":1,"message":"unfinished word"}]}}));
    poll(&mut app);
    crate::ui_editor::refresh_cached_editor_diagnostics(&mut app);
    assert!(std::sync::Arc::ptr_eq(&before, &app.cached_editor_diagnostics), "typing must not churn the diagnostic cache");
    assert!(app.cached_diag_lines.is_empty(), "no transient error marker in the gutter or overview");

    app.lsp_manager.diagnostic_publication.set_delay_ms(0);
    let mut redraw = false;
    poll_lsp(&mut app, &mut redraw);
    assert!(redraw);
    crate::ui_editor::refresh_cached_editor_diagnostics(&mut app);
    assert_eq!(app.cached_editor_diagnostics[0].message, "unfinished word");
    assert_eq!(&*app.cached_diag_lines, &[(0, 1)]);
}

#[test]
fn identical_diagnostics_on_new_text_remap_utf16_columns_in_editor_caches() {
    let (mut app, server, _directory, path) = fixture();
    app.lsp_manager.diagnostic_publication.set_delay_ms(0);
    for (version, text, expected_column) in [(2, "😀x", 1), (3, "abx", 2)] {
        let editor = app.editor_mut().unwrap();
        editor.rope = ropey::Rope::from_str(text);
        editor.content_version = version;
        app.lsp_manager.invalidate_document_feedback(&path);
        app.lsp_manager.sync_document_owned(&path, version, text.into());
        server.respond(json!({"method":"textDocument/publishDiagnostics","params":{
            "uri":lsp_types::Url::from_file_path(&path).unwrap(), "version":version - 1,
            "diagnostics":[{"range":{"start":{"line":0,"character":2},"end":{"line":0,"character":3}},
                "message":"same error", "severity":1}]}}));
        app.lsp_manager.poll();
        crate::ui_editor::refresh_cached_editor_diagnostics(&mut app);
        assert_eq!(app.cached_editor_diagnostics[0].col, expected_column);
        let cached = std::sync::Arc::clone(&app.cached_editor_diagnostics);
        let report = app.lsp_manager.current_diagnostic_context(&path).unwrap().to_vec();
        app.lsp_manager.receive_diagnostics(path.clone(), Some((version - 1) as i32), report, Instant::now());
        crate::ui_editor::refresh_cached_editor_diagnostics(&mut app);
        assert!(std::sync::Arc::ptr_eq(&cached, &app.cached_editor_diagnostics));
    }
}

#[test]
fn typing_queues_completion_after_latest_did_change_without_stalling_input() {
    let (mut app, server, _directory, path) = fixture();
    app.editor_mut().unwrap().rope = ropey::Rope::from_str("value");
    app.editor_mut().unwrap().set_cursor(0, 5);
    for _ in 0..30 {
        crate::input::handle_editor_key(&mut app, crossterm::event::KeyCode::Char('x'), crossterm::event::KeyModifiers::NONE);
    }
    assert!(server.take_messages().iter().all(|m| m["method"] != "textDocument/completion"));
    let version = app.editor().unwrap().content_version;
    app.wait_for_background(Duration::from_secs(5), |app| app.lsp_manager.last_notified_version.get(&path) == Some(&version));
    poll(&mut app);
    let messages = server.take_messages();
    let change = messages.iter().position(|m| m["method"] == "textDocument/didChange").unwrap();
    let completion = messages.iter().position(|m| m["method"] == "textDocument/completion").unwrap();
    assert!(change < completion);
    assert_eq!(messages[change]["params"]["contentChanges"][0]["text"], format!("value{}", "x".repeat(30)));
    assert_eq!(messages[completion]["params"]["position"]["character"], 35);
    assert_eq!(messages.iter().filter(|m| m["method"] == "textDocument/completion").count(), 1);
    assert_eq!(messages.iter().filter(|m| m["method"] == "textDocument/didChange").count(), 1);
}

#[test]
fn queued_snapshots_are_discarded_after_close_or_large_file_mode() {
    for close in [false, true] {
        let (mut app, server, _directory, path) = fixture();
        let editor = app.editor_mut().unwrap();
        editor.rope.insert(0, "changed ");
        editor.content_version += 1;
        let (version, rope) = (editor.content_version, editor.rope.clone());
        app.queue_lsp_document_sync(path.clone(), version, rope);
        if close { app.tabs.clear(); }
        else { app.editor_mut().unwrap().large_file_mode = true; }
        app.poll_background_results();
        assert!(server.take_messages().is_empty());
        assert_eq!(app.lsp_manager.last_notified_version[&path], 1);
    }
}

#[test]
fn document_highlights_reject_buffered_replies_after_cursor_moves_during_typing_delay() {
    let (mut app, server, _directory, _path) = fixture();
    poll(&mut app);
    let id = request_id(&server.take_messages(), "textDocument/documentHighlight");
    // Parse the reply first, as if the reader won the race with keyboard input.
    server.respond(json!({"id":id,"result":[{"range":{
        "start":{"line":0,"character":4},"end":{"line":0,"character":9}}}]}));
    app.editor_mut().unwrap().set_cursor(1, 5);
    app.last_edit_time = Instant::now();
    poll(&mut app);
    assert!(app.lsp_document_highlights.is_empty());
    assert!(app.lsp_manager.active_highlight_id.is_none());
    assert!(app.lsp_manager.highlight_request.is_none());
    assert!(server.take_messages().is_empty());

    app.last_edit_time = Instant::now() - Duration::from_secs(2);
    poll(&mut app);
    let messages = server.take_messages();
    let request = messages.iter().find(|m| m["method"] == "textDocument/documentHighlight").unwrap();
    assert_eq!(request["params"]["position"], json!({"line":1,"character":5}));
    server.respond(json!({"id":request["id"],"result":[{"range":{
        "start":{"line":1,"character":4},"end":{"line":1,"character":9}}}]}));
    poll(&mut app);
    assert_eq!(app.lsp_document_highlights, vec![(1, 4, 1, 9)]);
}

#[test]
fn highlights_are_scoped_to_file_even_when_cursor_and_content_versions_match() {
    let (mut app, server, directory, first) = fixture();
    poll(&mut app);
    let id = request_id(&server.take_messages(), "textDocument/documentHighlight");
    let second = directory.path().join("second.rs");
    let mut editor = Editor::test_with_text("let other = 2;\n");
    editor.content_version = 1;
    editor.file_path = Some(second.clone());
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 1;
    app.lsp_manager.sync_document_owned(&second, 1, "let other = 2;\n".into());
    server.respond(json!({"id":id,"result":[{"range":{
        "start":{"line":0,"character":4},"end":{"line":0,"character":9}}}]}));
    poll(&mut app);
    assert!(app.lsp_document_highlights.is_empty());
    let messages = server.take_messages();
    let request = messages.iter().find(|m| m["method"] == "textDocument/documentHighlight").unwrap();
    assert_eq!(request["params"]["textDocument"]["uri"], lsp_types::Url::from_file_path(&second).unwrap().as_str());
    assert_ne!(app.lsp_manager.highlight_request.as_ref().unwrap().0.target.path, first);
    app.lsp_manager.did_close(&second);
    assert!(app.lsp_manager.highlight_request.is_none());
    assert!(server.take_messages().iter().any(|m| m["method"] == "$/cancelRequest" && m["params"]["id"] == request["id"]));
}

#[test]
fn unsupported_and_empty_highlights_do_not_repeat_work_on_idle_frames() {
    for supported in [false, true] {
        let (mut app, server, _directory, _path) = fixture();
        app.lsp_manager.clients[0].install_capabilities(serde_json::from_value(json!({
            "documentHighlightProvider":supported})).unwrap());
        poll(&mut app);
        let initial = server.take_messages();
        if supported {
            let id = request_id(&initial, "textDocument/documentHighlight");
            server.respond(json!({"id":id,"result":null}));
            poll(&mut app);
        } else {
            assert!(initial.iter().all(|m| m["method"] != "textDocument/documentHighlight"));
        }
        let request = app.lsp_manager.highlight_request.as_ref().unwrap();
        let allocation = request.0.target.path.as_os_str().as_encoded_bytes().as_ptr();
        for _ in 0..100 { poll(&mut app); }
        assert_eq!(app.lsp_manager.highlight_request.as_ref().unwrap().0.target.path.as_os_str().as_encoded_bytes().as_ptr(), allocation);
        assert!(server.take_messages().is_empty());
    }
}

#[test]
fn highlight_cursor_cache_keeps_scalar_and_utf16_positions_distinct() {
    let (mut app, server, _directory, path) = fixture();
    let source = "/* 😀 */ let value = 1;";
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str(source);
    editor.content_version = 2;
    editor.set_cursor(0, 13);
    app.lsp_manager.sync_document_owned(&path, 2, source.into());
    poll(&mut app);
    let messages = server.take_messages();
    let request = messages.iter().find(|m| m["method"] == "textDocument/documentHighlight").unwrap();
    assert_eq!(request["params"]["position"]["character"], 14);
    for _ in 0..20 { poll(&mut app); }
    assert!(server.take_messages().is_empty());
    app.editor_mut().unwrap().set_cursor(0, 14);
    poll(&mut app);
    let messages = server.take_messages();
    assert!(messages.iter().any(|m| m["method"] == "$/cancelRequest" && m["params"]["id"] == request["id"]));
    let request = messages.iter().find(|m| m["method"] == "textDocument/documentHighlight").unwrap();
    assert_eq!(request["params"]["position"]["character"], 15);
}

#[test]
fn inlay_prefetch_accepts_pending_covered_replies_and_expands_when_viewport_grows() {
    let (mut app, server, _directory, _path) = fixture();
    poll(&mut app);
    let initial = server.take_messages();
    let id = request_id(&initial, "textDocument/inlayHint");
    app.editor_mut().unwrap().scroll_offset = 30;
    server.respond(json!({"id":id,"result":[hint("still covered")]}));
    poll(&mut app);
    assert_eq!(app.lsp_inlay_hints[0].label, "still covered");
    assert!(server.take_messages().is_empty());
    app.editor_mut().unwrap().viewport_height = 200;
    poll(&mut app);
    let messages = server.take_messages();
    let request = messages.iter().find(|m| m["method"] == "textDocument/inlayHint").unwrap();
    assert_eq!(request["params"]["range"]["start"]["line"], 0);
    assert_eq!(request["params"]["range"]["end"]["line"], 301);
}

#[test]
fn inlay_request_reaches_visible_lines_beyond_a_large_collapsed_fold() {
    let (mut app, server, _directory, _path) = fixture();
    app.editor_mut().unwrap().folded_ranges.insert(0, 250);
    poll(&mut app);
    let messages = server.take_messages();
    let request = messages.iter().find(|m| m["method"] == "textDocument/inlayHint").unwrap();
    assert_eq!(request["params"]["range"]["end"]["line"], 301);
    for offset in 250..=260 {
        app.editor_mut().unwrap().scroll_offset = offset;
        poll(&mut app);
    }
    assert!(server.take_messages().is_empty());
}

#[test]
fn pending_on_type_format_survives_another_documents_sync() {
    let (mut app, server, directory, path) = fixture();
    let other = directory.path().join("other.rs");
    let mut editor = Editor::test_with_text("other");
    editor.file_path = Some(other.clone());
    let (version, rope) = (editor.content_version, editor.rope.clone());
    app.tabs.push(Tab::new_file(editor));
    app.pending_lsp_on_type_format = Some((path.clone(), 0, 3, ";".into(), 4, true, 1));
    app.queue_lsp_document_sync(other, version, rope);
    app.poll_background_results();
    assert_eq!(app.pending_lsp_on_type_format.as_ref().unwrap().0, path);
    assert!(server.take_messages().iter().all(|m| m["method"] != "textDocument/onTypeFormatting"));
}

#[test]
fn manual_completion_and_signature_help_work_with_automatic_suggestions_disabled() {
    let (mut app, server, _directory, _path) = fixture();
    app.quick_suggestions = false;
    crate::input::handle_editor_key(&mut app, crossterm::event::KeyCode::Char(' '), crossterm::event::KeyModifiers::CONTROL);
    assert!(app.lsp_completion_active);
    assert!(server.take_messages().iter().any(|m| m["method"] == "textDocument/completion"));
    crate::input::handle_editor_key(&mut app, crossterm::event::KeyCode::Char(' '), crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT);
    assert!(app.lsp_signature_active);
    assert!(server.take_messages().iter().any(|m| m["method"] == "textDocument/signatureHelp"));
}

#[test]
fn escape_and_cursor_movement_discard_pending_suggestions_and_late_replies() {
    let (mut app, server, _directory, _path) = fixture();
    assert!(app.request_lsp_suggestions(false));
    let id = app.lsp_manager.active_completion_id.unwrap();
    crate::input_keyboard::handle_keyboard_event(&mut app, crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Esc, crossterm::event::KeyModifiers::NONE));
    server.respond(json!({"id":id,"result":[{"label":"late"}]}));
    poll(&mut app);
    assert!(!app.lsp_completion_active);
    assert!(app.lsp_manager.completion_items.is_empty());
    assert!(app.request_lsp_suggestions(false));
    let id = app.lsp_manager.active_completion_id.unwrap();
    app.editor_mut().unwrap().set_cursor(1, 0);
    server.respond(json!({"id":id,"result":[{"label":"wrong position"}]}));
    poll(&mut app);
    assert!(app.lsp_manager.completion_items.is_empty());
}

#[test]
fn delayed_rename_does_not_replace_newer_unsaved_text() {
    let (mut app, server, _directory, path) = fixture();
    assert!(app.lsp_manager.request_rename(&path, 0, 5, "renamed"));
    app.lsp_rename_pending = true;
    let id = app.lsp_manager.active_rename_id.unwrap();
    let editor = app.editor_mut().unwrap();
    editor.save_undo_public();
    editor.rope.insert(0, "// newer typing\n");
    let uri = lsp_types::Url::from_file_path(&path).unwrap();
    server.respond(json!({"id":id,"result":{"changes":{uri.as_str():[{"range":{"start":{"line":0,"character":4},"end":{"line":0,"character":9}},"newText":"renamed"}]}}}));
    poll(&mut app);
    assert!(app.editor().unwrap().rope.to_string().starts_with("// newer typing\n"));
    assert!(!app.editor().unwrap().rope.to_string().contains("renamed"));
}

#[test]
fn on_type_formatting_cannot_edit_another_tab_with_the_same_version() {
    let (mut app, _server, directory, path) = fixture();
    let mut editor = Editor::test_with_text("other text");
    editor.file_path = Some(directory.path().join("other.rs"));
    editor.content_version = app.editor().unwrap().content_version;
    app.lsp_on_type_format_target = Some(crate::background::LspDocumentTarget { path, content_version:editor.content_version });
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 1;
    app.lsp_manager.on_type_format_dirty = true;
    app.lsp_manager.on_type_format_edits = vec![lsp_types::TextEdit::new(
        lsp_types::Range::new(lsp_types::Position::new(0,0), lsp_types::Position::new(0,5)), "wrong tab".into())];
    poll(&mut app);
    assert_eq!(app.editor().unwrap().rope.to_string(), "other text");
}
