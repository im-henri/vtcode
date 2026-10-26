use crate::{
    app::{ActivePane, App, Tab},
    editor::Editor,
    lsp::{LspClient, LspEvent, TestLspServer},
};
use lsp_types::{Position, Range, SelectionRange};

fn fixture() -> (App, TestLspServer, tempfile::TempDir, i64) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.py");
    let mut editor = Editor::test_with_text("foo\nbar\nbaz");
    editor.file_path = Some(path.clone());
    editor.set_cursor(0, 1);
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    app.lsp_manager.clients.push(client);
    app.lsp_manager
        .sync_document_owned(&path, 0, "foo\nbar\nbaz".into());
    app.execute_editor_command("expand_selection");
    let request = app.lsp_manager.active_selection_range_id.unwrap();
    (app, server, directory, request)
}

fn response(server: &TestLspServer, request_id: i64, last_line: u32) {
    server.queue_event(LspEvent::SelectionRangeResponse {
        request_id,
        ranges: vec![SelectionRange {
            range: Range::new(Position::new(0, 0), Position::new(last_line, 3)),
            parent: None,
        }],
    });
}

fn poll(app: &mut App) {
    super::poll_lsp(app, &mut false);
}

#[test]
fn cached_selection_ranges_do_not_cross_tabs_with_the_same_cursor_position() {
    let (mut app, server, directory, request) = fixture();
    response(&server, request, 2);
    poll(&mut app);
    assert!(!app.lsp_selection_ranges.is_empty());
    let mut editor = Editor::test_with_text("other");
    editor.file_path = Some(directory.path().join("other.py"));
    editor.set_cursor(0, 1);
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 1;
    app.execute_editor_command("expand_selection");
    assert_eq!(app.editor().unwrap().selection_range(), Some((0, 0, 0, 5)));
}

#[test]
fn delayed_selection_response_is_rejected_after_the_document_changes() {
    let (mut app, server, _directory, request) = fixture();
    app.editor_mut().unwrap().select_all();
    app.editor_mut()
        .unwrap()
        .insert_char_replacing_selection('x');
    response(&server, request, 2);
    poll(&mut app);
    assert!(app.lsp_selection_ranges.is_empty());
}

#[test]
fn invalid_server_selection_ranges_cannot_create_out_of_bounds_positions() {
    let (mut app, server, _directory, request) = fixture();
    response(&server, request, 99);
    poll(&mut app);
    assert!(app.lsp_selection_ranges.is_empty());
}

#[test]
fn identical_request_ids_from_other_servers_cannot_supply_selection_ranges() {
    let (mut app, _server, _directory, request) = fixture();
    let (other, other_server) = LspClient::test_pair("other", &["*.other"]);
    app.lsp_manager.clients.push(other);
    response(&other_server, request, 2);
    poll(&mut app);
    assert!(app.lsp_selection_ranges.is_empty());
}

#[test]
fn valid_reply_survives_local_expansion_and_supports_repeated_expand_and_shrink() {
    let (mut app, server, _directory, request) = fixture();
    assert_eq!(
        app.editor().unwrap().selected_text().as_deref(),
        Some("foo")
    );
    response(&server, request, 2);
    poll(&mut app);
    server.take_messages();
    for _ in 0..5 {
        app.execute_editor_command("expand_selection");
        assert_eq!(app.editor().unwrap().selection_range(), Some((0, 0, 2, 3)));
        app.execute_editor_command("shrink_selection");
        assert_eq!(
            app.editor().unwrap().selected_text().as_deref(),
            Some("foo")
        );
    }
    for _ in 0..5 {
        app.execute_editor_command("expand_selection");
        assert_eq!(app.editor().unwrap().selection_range(), Some((0, 0, 2, 3)));
    }
    assert!(
        server
            .take_messages()
            .iter()
            .all(|message| message["method"] != "textDocument/selectionRange")
    );
}

#[test]
fn moving_the_caret_cancels_pending_selection_work() {
    let (mut app, server, _directory, request) = fixture();
    server.take_messages();
    app.editor_mut().unwrap().set_cursor(1, 1);
    response(&server, request, 2);
    poll(&mut app);
    assert!(app.lsp_selection_ranges.is_empty());
    assert!(server.take_messages().iter().any(
        |message| message["method"] == "$/cancelRequest" && message["params"]["id"] == request
    ));
    assert!(app.lsp_manager.active_selection_range_id.is_none());
    assert!(app.lsp_manager.active_selection_range_client.is_none());
}

#[test]
fn replacing_a_tab_at_the_same_path_and_version_invalidates_cached_ranges() {
    let (mut app, server, _directory, request) = fixture();
    response(&server, request, 2);
    poll(&mut app);
    let previous = app.editor().unwrap();
    let mut replacement = Editor::test_with_text("replacement");
    replacement.file_path = previous.file_path.clone();
    replacement.content_version = previous.content_version;
    replacement.set_cursor(previous.cursor_line, previous.cursor_col);
    app.tabs[0] = Tab::new_file(replacement);
    app.execute_editor_command("expand_selection");
    assert_eq!(
        app.editor().unwrap().selected_text().as_deref(),
        Some("replacement")
    );
}

#[test]
fn selection_requests_follow_unsaved_unicode_edits_and_decode_utf16_ranges() {
    let (mut app, server, _directory, _request) = fixture();
    server.take_messages();
    app.editor_mut().unwrap().select_all();
    app.editor_mut()
        .unwrap()
        .paste_plain_with_text("🙂word tail");
    app.editor_mut().unwrap().set_cursor(0, 3);
    app.execute_editor_command("expand_selection");
    let messages = server.take_messages();
    let changed = messages
        .iter()
        .position(|message| message["method"] == "textDocument/didChange")
        .unwrap();
    let requested = messages
        .iter()
        .position(|message| message["method"] == "textDocument/selectionRange")
        .unwrap();
    assert!(changed < requested);
    assert_eq!(
        messages[requested]["params"]["positions"][0]["character"],
        4
    );
    let request = app.lsp_manager.active_selection_range_id.unwrap();
    server.respond(serde_json::json!({"id":request, "result":[{
        "range":{"start":{"line":0,"character":2},"end":{"line":0,"character":11}}
    }]}));
    poll(&mut app);
    app.execute_editor_command("expand_selection");
    assert_eq!(app.editor().unwrap().selection_range(), Some((0, 1, 0, 10)));
    assert_eq!(
        app.editor().unwrap().selected_text().as_deref(),
        Some("word tail")
    );
}

#[test]
fn failed_selection_requests_are_retired_only_by_their_own_server() {
    let (mut app, _server, _directory, request) = fixture();
    assert!(
        !app.lsp_manager
            .finish_failed_request(1, request, "textDocument/selectionRange")
    );
    assert_eq!(app.lsp_manager.active_selection_range_id, Some(request));
    assert_eq!(app.lsp_manager.active_selection_range_client, Some(0));
    assert!(
        app.lsp_manager
            .finish_failed_request(0, request, "textDocument/selectionRange")
    );
    assert!(app.lsp_manager.active_selection_range_id.is_none());
    assert!(app.lsp_manager.active_selection_range_client.is_none());
}

#[test]
fn expanding_a_historical_revision_does_not_send_its_text_to_the_server() {
    let (mut app, server, _directory, _request) = fixture();
    server.take_messages();
    app.tabs[0].kind = crate::app::TabKind::GitRevision;
    app.editor_mut().unwrap().select_all();
    app.editor_mut()
        .unwrap()
        .paste_plain_with_text("historical");
    app.editor_mut().unwrap().set_cursor(0, 2);
    app.execute_editor_command("expand_selection");
    assert_eq!(
        app.editor().unwrap().selected_text().as_deref(),
        Some("historical")
    );
    assert!(
        server
            .take_messages()
            .iter()
            .all(|message| message["method"] == "$/cancelRequest")
    );
    assert!(app.lsp_selection_range_source.is_none());
}
