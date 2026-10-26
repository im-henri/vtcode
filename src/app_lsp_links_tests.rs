use super::*;
use crate::app::{ActivePane, Tab};
use crate::editor::Editor;
use crate::lsp::{LspClient, TestLspServer};
use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Modifier};
use serde_json::json;

fn draw(app: &mut App, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
    terminal
        .draw(|frame| crate::ui_editor::draw_editor(frame, app, Rect::new(0, 0, width, 8)))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn cell(buffer: &Buffer, text: &str) -> (u16, u16) {
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let found: String = (x..buffer.area.width)
                .map(|col| buffer[(col, y)].symbol())
                .collect();
            if found.starts_with(text) {
                return (x, y);
            }
        }
    }
    panic!("text not rendered: {text}");
}

fn fixture(text: &str) -> (App, TestLspServer, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sample.py");
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(path.clone());
    editor.content_version = 1;
    editor.cursor_line = editor.line_count() - 1;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    app.terminal_has_focus = true;
    app.show_line_numbers = false;
    app.show_minimap = false;
    app.show_inlay_hints = false;
    app.show_semantic_tokens = false;
    app.sticky_scroll = false;
    app.hover_enabled = false;
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    app.lsp_manager.clients.push(client);
    app.lsp_manager.sync_document_owned(&path, 1, text.into());
    server.take_messages();
    draw(&mut app, 60);
    (app, server, directory)
}

fn pointer(app: &mut App, at: (u16, u16), modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: at.0,
            row: at.1,
            modifiers,
        },
    );
    app.refresh_lsp_link_hover();
}

fn probe(server: &TestLspServer) -> serde_json::Value {
    server
        .take_messages()
        .into_iter()
        .find(|message| message["method"] == "textDocument/definition")
        .expect("definition probe")
}

fn resolve(app: &mut App, server: &TestLspServer, request: &serde_json::Value, found: bool) {
    let result = if found {
        json!([{
            "uri": request["params"]["textDocument"]["uri"],
            "range": {"start":{"line":2,"character":0},"end":{"line":2,"character":4}}
        }])
    } else {
        serde_json::Value::Null
    };
    server.respond(json!({"jsonrpc":"2.0", "id":request["id"], "result":result}));
    crate::app_lsp_poll::poll_lsp(app, &mut false);
}

#[test]
fn ctrl_hover_underlines_only_the_resolved_symbol_and_never_navigates() {
    let (mut app, server, _directory) = fixture("\tmeasurements = source\n\n");
    let before = draw(&mut app, 60);
    let at = cell(&before, "measurements");
    let cursor = (
        app.editor().unwrap().cursor_line,
        app.editor().unwrap().cursor_col,
    );
    pointer(&mut app, at, KeyModifiers::CONTROL);
    assert_eq!(
        app.hovered_editor_pos,
        Some((0, 1)),
        "tabs map to source columns"
    );
    assert!(
        app.lsp_link_hover_range().is_none(),
        "wait for the server to confirm a target"
    );
    let request = probe(&server);
    assert_eq!(
        request["params"]["position"],
        json!({"line":0,"character":1})
    );
    resolve(&mut app, &server, &request, true);
    assert_eq!(app.lsp_link_hover_range(), Some((0, 1, 13)));
    assert!(!app.lsp_manager.definition_dirty);
    assert!(!app.lsp_goto_pending);
    assert_eq!(
        (
            app.editor().unwrap().cursor_line,
            app.editor().unwrap().cursor_col
        ),
        cursor
    );
    let after = draw(&mut app, 60);
    for x in at.0..at.0 + 12 {
        assert!(after[(x, at.1)].modifier.contains(Modifier::UNDERLINED));
        assert_eq!(after[(x, at.1)].bg, before[(x, at.1)].bg);
        assert_eq!(after[(x, at.1)].fg, before[(x, at.1)].fg);
    }
    assert!(
        !after[(at.0 + 12, at.1)]
            .modifier
            .contains(Modifier::UNDERLINED)
    );
    pointer(&mut app, (at.0 + 5, at.1), KeyModifiers::CONTROL);
    assert!(
        !server
            .take_messages()
            .iter()
            .any(|message| message["method"] == "textDocument/definition"),
        "moving within one symbol reuses its result"
    );
    pointer(&mut app, (at.0 + 5, at.1), KeyModifiers::NONE);
    assert!(app.lsp_link_hover_range().is_none());
    let cleared = draw(&mut app, 60);
    assert!(!cleared[at].modifier.contains(Modifier::UNDERLINED));
}

#[test]
fn ctrl_hover_empty_results_and_unsupported_servers_do_not_look_clickable() {
    let (mut app, server, _directory) = fixture("unknown name\n");
    let at = cell(&draw(&mut app, 60), "unknown");
    pointer(&mut app, at, KeyModifiers::CONTROL);
    let request = probe(&server);
    resolve(&mut app, &server, &request, false);
    assert!(app.lsp_link_hover_range().is_none());
    for _ in 0..10 {
        app.refresh_lsp_link_hover();
    }
    assert!(
        server
            .take_messages()
            .iter()
            .all(|message| message["method"] != "textDocument/definition")
    );
    app.lsp_manager.clients[0].install_capabilities(Default::default());
    app.clear_lsp_link_hover();
    app.refresh_lsp_link_hover();
    assert!(app.lsp_link_hover_range().is_none());
    assert!(server.take_messages().is_empty());
}

#[test]
fn ctrl_hover_cancels_departed_symbols_and_ignores_late_replies() {
    let (mut app, server, _directory) = fixture("first second\n");
    let buffer = draw(&mut app, 60);
    pointer(&mut app, cell(&buffer, "first"), KeyModifiers::CONTROL);
    let first = probe(&server);
    pointer(&mut app, cell(&buffer, "second"), KeyModifiers::CONTROL);
    let messages = server.take_messages();
    assert!(
        messages
            .iter()
            .any(|message| message["method"] == "$/cancelRequest"
                && message["params"]["id"] == first["id"])
    );
    let second = messages
        .into_iter()
        .find(|message| message["method"] == "textDocument/definition")
        .unwrap();
    resolve(&mut app, &server, &first, true);
    assert!(app.lsp_link_hover_range().is_none());
    resolve(&mut app, &server, &second, true);
    assert_eq!(app.lsp_link_hover_range(), Some((0, 6, 12)));
    pointer(&mut app, (0, 7), KeyModifiers::CONTROL);
    assert!(app.lsp_link_hover_range().is_none());
}

#[test]
fn ctrl_hover_updates_when_control_changes_without_pointer_movement() {
    use crossterm::event::ModifierKeyCode::LeftControl;
    let (mut app, server, _directory) = fixture("measurements\n");
    let at = cell(&draw(&mut app, 60), "measurements");
    pointer(&mut app, at, KeyModifiers::NONE);
    // Switching views can clear ordinary hover state while the pointer stays put.
    app.hovered_editor_pos = None;
    let press = KeyEvent::new(KeyCode::Modifier(LeftControl), KeyModifiers::CONTROL);
    crate::input_keyboard::handle_keyboard_event(&mut app, press);
    app.refresh_lsp_link_hover();
    let request = probe(&server);
    resolve(&mut app, &server, &request, true);
    assert!(app.lsp_link_hover_range().is_some());
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new_with_kind(press.code, KeyModifiers::CONTROL, KeyEventKind::Release),
    );
    assert!(app.lsp_link_hover_range().is_none());
    assert_eq!(app.editor().unwrap().rope.to_string(), "measurements\n");
}

#[test]
fn ctrl_hover_document_links_need_no_definition_request() {
    let (mut app, server, _directory) = fixture("import source\n");
    app.lsp_document_links
        .push((0, 7, 13, "file:///source.py".into()));
    let at = cell(&draw(&mut app, 60), "source");
    pointer(&mut app, at, KeyModifiers::NONE);
    assert!(app.lsp_link_hover_range().is_none());
    pointer(&mut app, at, KeyModifiers::CONTROL);
    assert_eq!(app.lsp_link_hover_range(), Some((0, 7, 13)));
    assert!(server.take_messages().is_empty());
    app.editor_mut().unwrap().content_version += 1;
    assert!(
        app.lsp_link_hover_range().is_none(),
        "an edit invalidates the displayed range"
    );
}

#[test]
fn ctrl_hover_wrap_unicode_horizontal_scroll_and_fold_hit_the_painted_symbol() {
    for (text, wrap, scroll, folded, needle, expected) in [
        (
            "prefix prefix measurements\n",
            true,
            0,
            false,
            "measurements",
            (0, 14),
        ),
        (
            "界\tmeasurements\n",
            false,
            0,
            false,
            "measurements",
            (0, 2),
        ),
        (
            "padding\tmeasurements\n",
            false,
            5,
            false,
            "measurements",
            (0, 8),
        ),
        (
            "start\n hidden\n hidden\nmeasurements\n",
            false,
            0,
            true,
            "measurements",
            (3, 0),
        ),
    ] {
        let (mut app, server, _directory) = fixture(text);
        let editor = app.editor_mut().unwrap();
        editor.word_wrap = wrap;
        editor.horizontal_scroll = scroll;
        if folded {
            editor.folded_ranges.insert(0, 3);
        }
        let width = if wrap { 24 } else { 60 };
        let buffer = draw(&mut app, width);
        let at = cell(&buffer, needle);
        assert_eq!(
            editor_position_at(&mut app, at.0, at.1),
            Some(expected),
            "{text}"
        );
        pointer(&mut app, at, KeyModifiers::CONTROL);
        let request = probe(&server);
        resolve(&mut app, &server, &request, true);
        let after = draw(&mut app, width);
        assert!(after[at].modifier.contains(Modifier::UNDERLINED), "{text}");
    }
}

#[test]
fn ctrl_hover_probe_cannot_replace_an_explicit_navigation_result() {
    let (mut app, server, _directory) = fixture("measurements\n");
    let at = cell(&draw(&mut app, 60), "measurements");
    pointer(&mut app, at, KeyModifiers::CONTROL);
    let hover = probe(&server);
    let path = app.editor().unwrap().file_path.clone().unwrap();
    app.lsp_manager.request_definition(&path, 0, 0);
    let navigation = probe(&server);
    server.respond(json!({"jsonrpc":"2.0", "id":navigation["id"], "result":[{
        "uri":navigation["params"]["textDocument"]["uri"],
        "range":{"start":{"line":5,"character":0},"end":{"line":5,"character":5}}
    }]}));
    app.lsp_manager.poll();
    assert_eq!(app.lsp_manager.definition_locations[0].range.start.line, 5);
    resolve(&mut app, &server, &hover, true);
    assert_eq!(app.lsp_manager.definition_locations[0].range.start.line, 5);
    assert!(app.lsp_link_hover_range().is_some());
}

#[test]
fn ctrl_hover_stationary_mouse_modifier_reports_trigger_redraw() {
    let (mut app, _server, _directory) = fixture("measurements\n");
    let at = cell(&draw(&mut app, 60), "measurements");
    pointer(&mut app, at, KeyModifiers::NONE);
    let (_sender, receiver) = std::sync::mpsc::channel();
    let mut pump = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::NONE] {
        assert!(crate::app_event_loop::handle_mouse_input(
            &mut app,
            &mut pump,
            MouseEvent {
                kind: MouseEventKind::Moved,
                column: at.0,
                row: at.1,
                modifiers
            }
        ));
    }
}

#[test]
fn ctrl_hover_tab_switch_and_focus_loss_remove_the_range() {
    for lose_focus in [false, true] {
        let (mut app, server, _directory) = fixture("measurements\n");
        let at = cell(&draw(&mut app, 60), "measurements");
        pointer(&mut app, at, KeyModifiers::CONTROL);
        let request = probe(&server);
        if lose_focus {
            app.terminal_has_focus = false;
        } else {
            app.tabs[0] = Tab::new_file(Editor::test_with_text("another_file\n"));
        }
        app.refresh_lsp_link_hover();
        resolve(&mut app, &server, &request, true);
        assert!(app.lsp_link_hover_range().is_none());
    }
}

#[test]
fn ctrl_hover_waits_for_document_sync_and_uses_utf16_positions() {
    let (mut app, server, _directory) = fixture("😀 measurements\n");
    let path = app.editor().unwrap().file_path.clone().unwrap();
    let editor = app.editor_mut().unwrap();
    editor.rope.insert(0, "\t");
    editor.content_version = 2;
    let at = cell(&draw(&mut app, 60), "measurements");
    pointer(&mut app, at, KeyModifiers::CONTROL);
    assert!(
        server.take_messages().is_empty(),
        "sync is queued before probing edited text"
    );
    app.flush_lsp_document_syncs();
    app.refresh_lsp_link_hover();
    let messages = server.take_messages();
    let change = messages
        .iter()
        .position(|message| message["method"] == "textDocument/didChange")
        .unwrap();
    let probe = messages
        .iter()
        .position(|message| message["method"] == "textDocument/definition")
        .unwrap();
    assert!(change < probe);
    assert_eq!(
        messages[probe]["params"]["position"],
        json!({"line":0,"character":4})
    );
    assert_eq!(app.lsp_manager.last_notified_version.get(&path), Some(&2));
}

#[test]
fn ctrl_click_uses_the_same_source_position_as_the_underlined_symbol() {
    let (mut app, server, _directory) = fixture("\tmeasurements\n");
    let at = cell(&draw(&mut app, 60), "measurements");
    pointer(&mut app, at, KeyModifiers::CONTROL);
    let hover = probe(&server);
    resolve(&mut app, &server, &hover, true);
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: at.0,
            row: at.1,
            modifiers: KeyModifiers::CONTROL,
        },
    );
    let click = probe(&server);
    assert_eq!(click["params"]["position"], hover["params"]["position"]);
    assert!(app.lsp_goto_pending);
}
