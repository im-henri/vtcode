use super::poll_lsp;
use crate::app::{ActivePane, App, Tab};
use crate::editor::Editor;
use crate::lsp::{LspClient, LspEvent, TestLspServer};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const CONTENT: &str = "LSP hover documentation";

fn fixture() -> (App, TestLspServer, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sample.py");
    let text = "items = {}\nitems.items()\nthird = 3\n";
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(path.clone());
    editor.content_version = 1;
    editor.set_cursor(1, 5);
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    app.terminal_has_focus = true;
    app.show_line_numbers = false;
    app.show_minimap = false;
    app.show_inlay_hints = false;
    app.show_semantic_tokens = false;
    app.sticky_scroll = false;
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    app.lsp_manager.clients.push(client);
    app.lsp_manager.sync_document_owned(&path, 1, text.into());
    server.take_messages();
    rendered(&mut app);
    (app, server, directory)
}

fn rendered(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_editor::draw_editor(frame, app, Rect::new(0, 0, 80, 12));
            crate::ui_dialogs::draw_lsp_hover(frame, app);
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn poll(app: &mut App) -> bool {
    let mut redraw = false;
    poll_lsp(app, &mut redraw);
    redraw
}

fn mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
    poll(app);
}

fn hover_at(app: &mut App, col: u16) {
    let area = app.code_area.unwrap();
    mouse(app, MouseEventKind::Moved, area.x + col, area.y);
}

fn request(app: &mut App, server: &TestLspServer) -> Value {
    assert!(
        app.lsp_hover_request_time.is_some(),
        "pointer movement starts the hover timer"
    );
    app.lsp_hover_request_time = Some(Instant::now() - Duration::from_millis(501));
    poll(app);
    server
        .take_messages()
        .into_iter()
        .find(|message| message["method"] == "textDocument/hover")
        .expect("hover request after the debounce")
}

fn reply(app: &mut App, server: &TestLspServer, request: &Value) {
    server.respond(json!({"id":request["id"],"result":{"contents":CONTENT}}));
    assert!(
        poll(app),
        "a hover response must redraw even with no further input"
    );
    assert!(app.lsp_manager.active_hover_id.is_none());
    assert!(rendered(app).contains(CONTENT));
}

fn show(app: &mut App, server: &TestLspServer) {
    hover_at(app, 0);
    let request = request(app, server);
    reply(app, server, &request);
}

fn assert_dismissed(app: &mut App, server: &TestLspServer) {
    assert!(!app.lsp_hover_active);
    assert!(app.lsp_hover_suppressed);
    assert!(app.lsp_hover_request_time.is_none());
    assert!(app.lsp_hover_pending_pos.is_none());
    assert!(app.lsp_manager.active_hover_id.is_none());
    assert!(app.lsp_manager.hover_content.is_none());
    for _ in 0..3 {
        poll(app);
    }
    assert!(
        app.lsp_hover_request_time.is_none(),
        "the stationary mouse must not restart the timer"
    );
    assert!(
        server
            .take_messages()
            .iter()
            .all(|message| message["method"] != "textDocument/hover")
    );
    assert!(!rendered(app).contains(CONTENT));
}

#[test]
fn hover_survives_full_workbench_redraws() {
    let (mut app, server, _directory) = fixture();
    app.show_line_numbers = true;
    app.show_minimap = true;
    app.sticky_scroll = true;
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    hover_at(&mut app, 1);
    assert_eq!(app.hovered_editor_pos, Some((0, 1)));
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let request = request(&mut app, &server);
    reply(&mut app, &server, &request);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains(CONTENT));
}

#[test]
fn hover_synchronizes_recent_unsaved_text_before_requesting_documentation() {
    let (mut app, server, _directory) = fixture();
    let editor = app.editor_mut().unwrap();
    editor.set_cursor(0, 0);
    editor.insert_char('x');
    let version = editor.content_version;
    let path = editor.file_path.clone().unwrap();
    app.last_edit_time = Instant::now();
    rendered(&mut app);
    hover_at(&mut app, 1);
    app.lsp_hover_request_time = Some(Instant::now() - Duration::from_millis(501));
    poll(&mut app);
    let messages = server.take_messages();
    let change = messages
        .iter()
        .position(|m| m["method"] == "textDocument/didChange")
        .expect("hover must synchronize the text before the editing quiet period expires");
    let hover = messages
        .iter()
        .position(|m| m["method"] == "textDocument/hover")
        .unwrap();
    assert!(change < hover);
    assert_eq!(
        app.lsp_manager.last_notified_version.get(&path),
        Some(&version)
    );
    reply(&mut app, &server, &messages[hover]);
}

#[test]
fn stationary_hover_recovers_when_the_server_finishes_initializing() {
    let (mut app, server, _directory) = fixture();
    app.lsp_manager.clients[0].initialized = false;
    hover_at(&mut app, 1);
    if app.lsp_hover_request_time.is_some() {
        app.lsp_hover_request_time = Some(Instant::now() - Duration::from_millis(501));
    }
    poll(&mut app);
    assert!(!app.lsp_hover_active);
    assert!(server.take_messages().is_empty());
    server.queue_event(LspEvent::Initialized {
        capabilities: Box::new(None),
    });
    poll(&mut app);
    let request = request(&mut app, &server);
    reply(&mut app, &server, &request);
}

#[test]
fn unsupported_or_unrelated_servers_do_not_start_a_hover_timer() {
    let (mut app, server, _directory) = fixture();
    app.lsp_manager.clients[0].install_capabilities(Default::default());
    hover_at(&mut app, 1);
    assert!(app.lsp_hover_request_time.is_none());
    assert!(!app.lsp_hover_active);
    server.take_messages();
    for _ in 0..10 {
        poll(&mut app);
    }
    assert!(server.take_messages().is_empty());

    let (unrelated, other_server) = LspClient::test_pair("c", &["*.c"]);
    app.lsp_manager.clients = vec![unrelated];
    hover_at(&mut app, 2);
    assert!(app.lsp_hover_request_time.is_none());
    assert!(other_server.take_messages().is_empty());
}

#[test]
fn hover_uses_a_ready_provider_when_another_matching_server_cannot_hover() {
    for starting in [false, true] {
        let (mut app, server, _directory) = fixture();
        let (mut unavailable, other_server) = LspClient::test_pair("other", &["*.py"]);
        unavailable.initialized = !starting;
        if !starting {
            unavailable.install_capabilities(Default::default());
        }
        unavailable
            .did_open(
                app.editor().unwrap().file_path.as_deref().unwrap(),
                "python",
                &app.editor().unwrap().rope.to_string(),
            )
            .unwrap();
        other_server.take_messages();
        app.lsp_manager.clients.insert(0, unavailable);
        hover_at(&mut app, 1);
        let request = request(&mut app, &server);
        reply(&mut app, &server, &request);
        assert!(
            other_server
                .take_messages()
                .iter()
                .all(|m| m["method"] != "textDocument/hover")
        );
    }
}

fn real_hover(language: &str) {
    use crate::lsp::{LspManager, LspServerConfig};
    let directory = tempfile::tempdir().unwrap();
    let python = language == "python";
    let extension = if python { "py" } else { language };
    let path = directory.path().join(format!("hover.{extension}"));
    let text = if python { "x = 42\n" } else { "int x = 42;\n" };
    std::fs::write(&path, text).unwrap();
    let mut app = App::new(directory.path().to_path_buf());
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(path.clone());
    editor.content_version = 1;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    app.terminal_has_focus = true;
    app.lsp_manager =
        LspManager::new_with_wake(directory.path().to_path_buf(), app.background.wake_handle());
    app.lsp_manager.register_config(LspServerConfig {
        language: language.into(),
        command: std::env::var(if python {
            "VTCODE_PYRIGHT_LANGSERVER"
        } else {
            "VTCODE_CLANGD"
        })
        .unwrap_or_else(|_| {
            if python {
                "pyright-langserver"
            } else {
                "clangd"
            }
            .into()
        }),
        args: if python {
            vec!["--stdio".into()]
        } else {
            vec![
                "--background-index=false".into(),
                "--clang-tidy=false".into(),
            ]
        },
        file_patterns: vec![format!("*.{extension}")],
        settings: None,
        init_options: None,
    });
    app.lsp_manager
        .sync_document(&path, 1, app.tabs[0].editor.rope.clone());
    assert!(
        !app.lsp_manager.clients.is_empty(),
        "{:?}",
        app.lsp_manager.startup_errors
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let symbol = if python { 0 } else { 4 };
    hover_at(&mut app, symbol);

    let start = Instant::now();
    while app.lsp_manager.hover_content.is_none() {
        poll(&mut app);
        // Exercise the real delay and the wake from server initialization.
        assert!(
            start.elapsed() < Duration::from_secs(25),
            "{language}: hover failed: {:?}",
            app.lsp_manager.request_errors
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let original = app.lsp_manager.hover_content.clone().unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    assert!(app.lsp_hover_active);

    // An unsaved rename makes the old on-disk position empty. The 500ms hover
    // delay is shorter than passive synchronization; no other request may
    // accidentally send the changed snapshot first.
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );
    assert!(app.lsp_hover_suppressed);
    let editor = app.editor_mut().unwrap();
    let symbol = symbol as usize;
    editor.rope.remove(symbol..symbol + 1);
    editor.rope.insert(symbol, "measurements");
    editor.content_version += 1;
    app.last_edit_time = Instant::now();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    hover_at(&mut app, symbol as u16 + 8);
    app.lsp_hover_request_time = Some(Instant::now() - Duration::from_millis(501));
    poll(&mut app);
    let start = Instant::now();
    while app.lsp_manager.active_hover_id.is_some() {
        poll(&mut app);
        assert!(
            start.elapsed() < Duration::from_secs(25),
            "{language}: edited hover timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let content = app
        .lsp_manager
        .hover_content
        .as_deref()
        .expect("documentation for the unsaved name");
    assert!(content.contains("measurements"), "{language}: {content}");
    assert_ne!(content, original);
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let rendered: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(rendered.contains("measurements"));
    app.lsp_hover_active = false;
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let without_popup: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert_ne!(
        rendered, without_popup,
        "the documentation must be painted over the workbench"
    );
    app.lsp_manager.shutdown_all();
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_lsp_pyright_mouse_hover_after_startup_and_unsaved_rename() {
    real_hover("python");
}

#[test]
#[ignore = "requires installed clangd"]
fn real_lsp_clangd_mouse_hover_after_startup_and_unsaved_rename() {
    real_hover("c");
    real_hover("cpp");
}

#[test]
fn keyboard_navigation_typing_and_escape_dismiss_visible_hover() {
    for code in [
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Char('x'),
        KeyCode::Esc,
    ] {
        let (mut app, server, _directory) = fixture();
        show(&mut app, &server);
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(code, KeyModifiers::NONE),
        );
        assert_dismissed(&mut app, &server);
    }
}

#[test]
fn keyboard_movement_cancels_the_hover_debounce_before_a_request_is_sent() {
    let (mut app, server, _directory) = fixture();
    hover_at(&mut app, 0);
    assert!(app.lsp_hover_request_time.is_some());
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
    );
    assert_eq!(app.editor().unwrap().cursor_line, 2);
    assert_dismissed(&mut app, &server);
}

#[test]
fn cancelled_in_flight_hover_cannot_reappear_or_replace_a_new_hover() {
    let (mut app, server, _directory) = fixture();
    hover_at(&mut app, 0);
    let old = request(&mut app, &server);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
    );
    assert!(
        server
            .take_messages()
            .iter()
            .any(|message| message["method"] == "$/cancelRequest"
                && message["params"]["id"] == old["id"])
    );
    server.queue_event(LspEvent::HoverResponse {
        request_id: old["id"].as_i64().unwrap(),
        contents: Some("late old reply".into()),
    });
    assert_dismissed(&mut app, &server);

    hover_at(&mut app, 1);
    let fresh = request(&mut app, &server);
    server.queue_event(LspEvent::HoverResponse {
        request_id: old["id"].as_i64().unwrap(),
        contents: Some("late old reply".into()),
    });
    poll(&mut app);
    assert!(app.lsp_manager.hover_content.is_none());
    assert_eq!(app.lsp_manager.active_hover_id, fresh["id"].as_i64());
    reply(&mut app, &server, &fresh);
}

#[test]
fn only_real_mouse_movement_rearms_hover_even_within_the_same_word() {
    let (mut app, server, _directory) = fixture();
    show(&mut app, &server);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
    );
    hover_at(&mut app, 0); // A duplicate/stationary pointer report must not rearm.
    assert_dismissed(&mut app, &server);
    hover_at(&mut app, 1); // Another character of "items" is sufficient.
    assert!(!app.lsp_hover_suppressed);
    let request = request(&mut app, &server);
    assert_eq!(
        request["params"]["position"],
        json!({"line":0,"character":1})
    );
    reply(&mut app, &server, &request);
}

#[test]
fn returning_to_the_exact_same_character_can_show_hover_again() {
    let (mut app, server, _directory) = fixture();
    show(&mut app, &server);
    let area = app.code_area.unwrap();
    mouse(&mut app, MouseEventKind::Moved, area.right() - 1, area.y);
    assert!(app.lsp_hover_pending_pos.is_none());
    assert!(!app.lsp_hover_active);
    hover_at(&mut app, 0);
    let request = request(&mut app, &server);
    assert_eq!(
        request["params"]["position"],
        json!({"line":0,"character":0})
    );
    reply(&mut app, &server, &request);
}

#[test]
fn modifier_only_and_key_release_events_do_not_dismiss_hover() {
    let (mut app, server, _directory) = fixture();
    show(&mut app, &server);
    for key in [
        KeyEvent::new(
            KeyCode::Modifier(ModifierKeyCode::LeftShift),
            KeyModifiers::SHIFT,
        ),
        KeyEvent::new_with_kind(KeyCode::Right, KeyModifiers::NONE, KeyEventKind::Release),
    ] {
        crate::input_keyboard::handle_keyboard_event(&mut app, key);
        poll(&mut app);
        assert!(rendered(&mut app).contains(CONTENT));
    }
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new_with_kind(KeyCode::Right, KeyModifiers::NONE, KeyEventKind::Repeat),
    );
    assert_dismissed(&mut app, &server);
}

#[test]
fn clicking_or_scrolling_dismisses_hover_until_the_pointer_moves() {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::ScrollDown,
    ] {
        let (mut app, server, _directory) = fixture();
        show(&mut app, &server);
        let area = app.code_area.unwrap();
        mouse(&mut app, kind, area.x, area.y);
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            area.x,
            area.y,
        );
        assert_dismissed(&mut app, &server);
    }
}

#[test]
fn tab_switch_cleanup_retires_the_hover_timer_and_request() {
    let (mut app, server, _directory) = fixture();
    hover_at(&mut app, 0);
    let old = request(&mut app, &server);
    app.clear_transient_editor_state();
    server.queue_event(LspEvent::HoverResponse {
        request_id: old["id"].as_i64().unwrap(),
        contents: Some(CONTENT.into()),
    });
    assert_dismissed(&mut app, &server);
}
