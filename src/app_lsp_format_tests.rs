use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    lsp::{LspClient, LspEvent, LspManager, LspServerConfig, TestLspServer},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};

fn config(language: &str, command: &str, pattern: &str) -> LspServerConfig {
    LspServerConfig {
        language: language.into(),
        command: command.into(),
        args: vec![],
        file_patterns: vec![pattern.into()],
        settings: None,
        init_options: None,
    }
}

fn fixture(initialized: bool) -> (tempfile::TempDir, App, TestLspServer, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("example.c");
    let source = "int main(){return 0;}\n";
    std::fs::write(&path, source).unwrap();
    let mut app = App::new(dir.path().to_path_buf());
    let mut editor = Editor::test_with_text(source);
    editor.file_path = Some(path.clone());
    editor.content_version = 1;
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.formatters.insert(crate::formatters::language(app.editor().unwrap()), Default::default());
    app.lsp_manager = LspManager::new(dir.path().to_path_buf());
    app.lsp_manager
        .register_config(config("c", "test-clangd", "*.c"));
    let (mut client, server) = LspClient::test_pair("c", &["*.c"]);
    client.initialized = initialized;
    app.lsp_manager.clients.push(client);
    (dir, app, server, path)
}

fn poll(app: &mut App) {
    crate::app_lsp_poll::poll_lsp(app, &mut false);
}

#[test]
fn repeated_save_joins_pending_document_formatting_instead_of_writing_unformatted_copies() {
    let (_dir, mut app, server, path) = fixture(false);
    app.editor_mut().unwrap().open_file(&path).unwrap();
    app.format_on_save = true;
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    app.editor_mut().unwrap().insert_char('A');
    let text = app.editor().unwrap().rope.to_string();
    for _ in 0..64 { app.save_current_file(); }
    assert!(app.lsp_format_pending);
    assert_eq!(app.lsp_save_after_format_path.as_ref(), Some(&path));
    assert!(app.pending_file_saves.is_empty() && app.deferred_file_saves.is_empty());
    assert!(app.formatting.error.is_none());
    server.queue_event(LspEvent::Initialized { capabilities: Box::new(Some(
        serde_json::from_value(json!({"documentFormattingProvider":true,"textDocumentSync":1})).unwrap())) });
    poll(&mut app);
    let messages = server.take_messages();
    assert_eq!(messages.iter().filter(|message| message["method"] == "textDocument/formatting").count(), 1);
    server.respond(json!({"id":request(&messages,"textDocument/formatting"),"result":[]}));
    poll(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| app.pending_file_saves.is_empty());
    assert!(app.save_conflict.is_none());
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    assert!(!app.editor().unwrap().modified);
}

#[test]
fn same_path_and_version_do_not_join_or_apply_a_closed_documents_format_request() {
    let (_dir, mut app, server, path) = fixture(true);
    let original = std::fs::read_to_string(&path).unwrap();
    app.execute_palette_command("format_document");
    let messages = server.take_messages();
    let id = request(&messages, "textDocument/formatting");
    let version = app.editor().unwrap().content_version;
    let mut replacement = Editor::test_with_text("new document\n");
    replacement.file_path = Some(path.clone());
    replacement.content_version = version;
    app.tabs = vec![Tab::new_file(replacement)];
    assert!(!app.request_format(false, true));
    assert!(app.lsp_save_after_format_path.is_none());
    server.respond(json!({"id":id,"result":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"OLD FORMAT "}]}));
    poll(&mut app);
    assert_eq!(app.editor().unwrap().rope, "new document\n");
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    assert!(app.pending_file_saves.is_empty());
    assert!(app.formatting.error.is_some());
}

fn request(messages: &[Value], method: &str) -> i64 {
    messages
        .iter()
        .find(|message| message["method"] == method)
        .unwrap()["id"]
        .as_i64()
        .unwrap()
}

fn render(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn format_shortcut_synchronizes_current_typing_before_request_and_is_undoable() {
    let (_dir, mut app, server, path) = fixture(true);
    let old = app.editor().unwrap().rope.clone();
    app.lsp_manager.sync_document(&path, 1, old);
    server.take_messages();
    let editor = app.editor_mut().unwrap();
    editor.save_undo_public();
    editor.rope.insert(0, "// newest 😀\n");
    let before = editor.rope.to_string();
    let version = editor.content_version;
    app.last_edit_time = Instant::now();
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT | KeyModifiers::ALT),
    );
    let messages = server.take_messages();
    assert_eq!(messages[0]["method"], "textDocument/didChange");
    assert_eq!(messages[1]["method"], "textDocument/formatting");
    assert_eq!(
        app.lsp_manager
            .format_request_target
            .as_ref()
            .unwrap()
            .content_version,
        version
    );
    let id = request(&messages, "textDocument/formatting");
    assert!(render(&mut app, 100, 30).contains("Formatting…"));
    server.respond(json!({"id":id,"result":[{"range":{"start":{"line":1,"character":10},"end":{"line":1,"character":10}},"newText":" "}]}));
    poll(&mut app);
    assert!(!app.lsp_format_pending);
    assert!(app.formatting.error.is_none());
    assert!(
        app.editor()
            .unwrap()
            .rope
            .to_string()
            .starts_with("// newest 😀\n")
    );
    assert!(render(&mut app, 100, 30).contains("Document formatted"));
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), before);
}

#[test]
fn formatting_waits_for_initialize_and_did_open_without_requiring_a_retry() {
    let (_dir, mut app, server, _path) = fixture(false);
    app.execute_palette_command("format_document");
    assert!(app.lsp_format_pending);
    assert!(app.formatting.error.is_none());
    assert!(server.take_messages().is_empty());
    server.queue_event(LspEvent::Initialized {
        capabilities: Box::new(Some(
            serde_json::from_value(json!({"documentFormattingProvider":true,"textDocumentSync":1}))
                .unwrap(),
        )),
    });
    poll(&mut app);
    let messages = server.take_messages();
    let opened = messages
        .iter()
        .position(|m| m["method"] == "textDocument/didOpen")
        .unwrap();
    let formatted = messages
        .iter()
        .position(|m| m["method"] == "textDocument/formatting")
        .unwrap();
    assert!(opened < formatted);
    server.respond(json!({"id":request(&messages,"textDocument/formatting"),"result":null}));
    poll(&mut app);
    assert!(!app.lsp_format_pending);
    assert!(app.formatting.error.is_none());
    assert_eq!(
        app.formatting.feedback.as_ref().unwrap().0,
        "No formatting changes"
    );
}

#[test]
fn format_selection_synchronizes_and_uses_utf16_positions() {
    let (_dir, mut app, server, _path) = fixture(true);
    let editor = app.editor_mut().unwrap();
    editor.rope = ropey::Rope::from_str("//😀abc xyz\n");
    editor.selection_anchor = Some((0, 3));
    editor.cursor_col = 6;
    app.execute_palette_command("format_selection");
    let messages = server.take_messages();
    let request = messages
        .iter()
        .find(|m| m["method"] == "textDocument/rangeFormatting")
        .unwrap();
    assert_eq!(messages[0]["method"], "textDocument/didOpen");
    assert_eq!(request["params"]["range"]["start"]["character"], 4);
    assert_eq!(request["params"]["range"]["end"]["character"], 7);
}

#[test]
fn pyright_without_formatting_shows_a_dismissible_popup_and_settings_action() {
    let (_dir, mut app, server, _path) = fixture(true);
    app.lsp_manager.configs[0].command = "pyright-langserver".into();
    app.lsp_manager.configs[0].language = "python".into();
    app.lsp_manager.clients[0].language = "python".into();
    app.lsp_manager.clients[0]
        .install_capabilities(serde_json::from_value(json!({"hoverProvider":true})).unwrap());
    app.show_panel = false;
    app.execute_palette_command("format_document");
    assert!(!app.lsp_format_pending);
    assert!(
        server
            .take_messages()
            .iter()
            .all(|message| message["method"] != "textDocument/formatting")
    );
    for (width, height) in [(100, 30), (60, 24)] {
        let screen = render(&mut app, width, height);
        assert!(screen.contains("Formatting failed"));
        assert!(screen.contains("Pyright"));
        assert!(screen.contains("Dismiss"));
        assert_eq!(app.modal.focus, 1, "Enter dismisses the error");
    }
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.formatting.error.is_none());
    app.execute_palette_command("format_document");
    render(&mut app, 100, 30);
    let button = app
        .modal
        .buttons
        .iter()
        .find(|(_, index)| *index == 0)
        .unwrap()
        .0;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x,
            row: button.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.formatting.error.is_none());
    assert!(app.formatter_settings.active);
}

#[test]
fn server_and_invalid_edit_errors_are_visible_instead_of_no_changes() {
    for response in [
        json!({"error":{"code":-32603,"message":"Bad .clang-format: unknown option Example"}}),
        json!({"result":{"not":"text edits"}}),
        json!({"result":[{"range":{"start":{"line":999,"character":0},"end":{"line":999,"character":1}},"newText":"bad"}]}),
        json!({"result":[
            {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":5}},"newText":"one"},
            {"range":{"start":{"line":0,"character":2},"end":{"line":0,"character":4}},"newText":"two"}]}),
    ] {
        let (_dir, mut app, server, _path) = fixture(true);
        let original = app.editor().unwrap().rope.to_string();
        app.execute_palette_command("format_document");
        let mut response = response;
        response["id"] = json!(request(&server.take_messages(), "textDocument/formatting"));
        server.respond(response.clone());
        poll(&mut app);
        assert!(!app.lsp_format_pending);
        assert!(app.formatting.error.is_some(), "{response}");
        assert_eq!(app.editor().unwrap().rope.to_string(), original);
        assert!(app.formatting.feedback.is_none());
        assert!(
            !app.notification_history
                .iter()
                .any(|notice| notice.0 == "No formatting changes")
        );
        if response.get("error").is_some() {
            assert!(
                app.formatting
                    .error
                    .as_ref()
                    .unwrap()
                    .contains("Bad .clang-format")
            );
        }
        assert!(render(&mut app, 100, 30).contains("Formatting failed"));
    }
}

#[test]
fn timeout_cancels_the_request_and_late_reply_cannot_change_the_document() {
    let (_dir, mut app, server, _path) = fixture(true);
    app.execute_palette_command("format_document");
    let id = request(&server.take_messages(), "textDocument/formatting");
    app.formatting.pending.as_mut().unwrap().deadline = Instant::now() - Duration::from_millis(1);
    assert_eq!(app.formatting.next_wait(), Some(Duration::ZERO));
    poll(&mut app);
    assert!(!app.lsp_format_pending);
    assert!(
        app.formatting
            .error
            .as_ref()
            .unwrap()
            .contains("30 seconds")
    );
    assert!(
        server
            .take_messages()
            .iter()
            .any(|m| m["method"] == "$/cancelRequest" && m["params"]["id"] == id)
    );
    server.respond(json!({"id":id,"result":[]}));
    poll(&mut app);
    assert!(app.formatting.error.is_some());
    assert!(app.formatting.feedback.is_none());
}

#[test]
fn typing_after_request_or_server_exit_clears_pending_formatting() {
    for stop_server in [false, true] {
        let (_dir, mut app, server, _path) = fixture(true);
        app.execute_palette_command("format_document");
        server.take_messages();
        if stop_server {
            server.queue_event(LspEvent::ServerError(()));
        } else {
            let editor = app.editor_mut().unwrap();
            editor.save_undo_public();
            editor.rope.insert(0, "// typed\n");
        }
        poll(&mut app);
        assert!(!app.lsp_format_pending);
        let error = app.formatting.error.as_ref().unwrap();
        assert!(
            error.contains(if stop_server { "stopped" } else { "changed" }),
            "{error}"
        );
    }
}

#[test]
fn formatting_does_not_follow_focus_into_a_different_editor() {
    let (dir, mut app, server, _path) = fixture(true);
    app.execute_palette_command("format_document");
    let id = request(&server.take_messages(), "textDocument/formatting");
    let mut second = Editor::test_with_text("other document");
    second.file_path = Some(dir.path().join("other.c"));
    app.tabs.push(Tab::new_file(second));
    app.active_tab = 1;
    server.respond(json!({"id":id,"result":[{"range":{"start":{"line":0,"character":10},"end":{"line":0,"character":10}},"newText":" "}]}));
    poll(&mut app);
    assert_eq!(app.tabs[1].editor.rope.to_string(), "other document");
    assert!(app.tabs[0].editor.rope.to_string().contains("() {return"));
    assert_eq!(app.active_tab, 1);
}

#[test]
fn format_on_save_failure_saves_current_text_and_shows_the_reason() {
    let (_dir, mut app, server, path) = fixture(true);
    app.format_on_save = true;
    app.editor_mut().unwrap().modified = true;
    app.save_current_file();
    let id = request(&server.take_messages(), "textDocument/formatting");
    server.respond(json!({"id":id,"error":{"code":-32603,"message":"invalid style"}}));
    poll(&mut app);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.pending_file_saves.contains_key(&path)
    });
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        app.editor().unwrap().rope.to_string()
    );
    assert!(
        app.formatting
            .error
            .as_ref()
            .unwrap()
            .contains("invalid style")
    );
    assert!(
        app.formatting
            .error
            .as_ref()
            .unwrap()
            .contains("without formatting")
    );
}

#[test]
fn unavailable_targets_and_missing_config_always_explain_the_failure() {
    for scenario in [
        "no file",
        "untitled",
        "read only",
        "disabled",
        "no selection",
        "no server",
    ] {
        let (_dir, mut app, _server, _path) = fixture(true);
        match scenario {
            "no file" => app.tabs.clear(),
            "untitled" => app.editor_mut().unwrap().file_path = None,
            "read only" => app.editor_mut().unwrap().read_only = true,
            "disabled" => app.lsp_enabled = false,
            "no server" => {
                app.lsp_manager.clients.clear();
                app.lsp_manager.configs.clear();
            }
            _ => {}
        }
        app.execute_palette_command(if scenario == "no selection" {
            "format_selection"
        } else {
            "format_document"
        });
        assert!(!app.lsp_format_pending, "{scenario}");
        assert!(app.formatting.error.is_some(), "{scenario}");
    }
}

fn real_app_format(language: &str, command: &str, args: Vec<String>) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let ext = if language == "python" { "py" } else { language };
    let path = dir.path().join(format!("example.{ext}"));
    let source = if language == "python" {
        "value= 42\n"
    } else {
        "int main(){int value=42;return value;}\n"
    };
    std::fs::write(&path, source).unwrap();
    let mut app = App::new(dir.path().to_path_buf());
    app.lsp_manager =
        LspManager::new_with_wake(dir.path().to_path_buf(), app.background.wake_handle());
    let mut cfg = config(language, command, &format!("*.{ext}"));
    cfg.args = args;
    app.lsp_manager.register_config(cfg);
    let mut editor = Editor::test_with_text(source);
    editor.file_path = Some(path.clone());
    editor.content_version = 1;
    app.tabs = vec![Tab::new_file(editor)];
    app.lsp_manager
        .sync_document(&path, 1, app.tabs[0].editor.rope.clone());
    // Real startup plus typing immediately before Shift+Alt+F reproduces the
    // old stale-buffer request; no debounce sleep or retry is allowed here.
    let editor = &mut app.tabs[0].editor;
    editor.save_undo_public();
    editor.rope.insert(
        0,
        if language == "python" {
            "# newest 😀\n"
        } else {
            "// newest 😀\n"
        },
    );
    let before = editor.rope.to_string();
    app.formatters.insert(crate::formatters::language(app.editor().unwrap()), Default::default());
    app.execute_palette_command("format_document");
    let start = Instant::now();
    while app.lsp_format_pending {
        poll(&mut app);
        assert!(start.elapsed() < Duration::from_secs(35), "formatting hung");
        std::thread::sleep(Duration::from_millis(10));
    }
    if language == "python" {
        assert!(app.formatting.error.as_ref().unwrap().contains("Pyright"));
        assert_eq!(app.editor().unwrap().rope.to_string(), before);
        assert!(app.lsp_manager.clients[0].initialized);
    } else {
        assert!(app.formatting.error.is_none(), "{:?}", app.formatting.error);
        assert!(
            app.editor()
                .unwrap()
                .rope
                .to_string()
                .contains("int value = 42;")
        );
        assert!(
            app.editor()
                .unwrap()
                .rope
                .to_string()
                .starts_with("// newest 😀")
        );
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), before);
    }
    (dir, app)
}

#[test]
#[ignore = "requires installed clangd"]
fn real_format_document_clangd_c_and_cpp_after_immediate_typing() {
    for language in ["c", "cpp"] {
        let (_dir, mut app) = real_app_format(
            language,
            "clangd",
            vec![
                "--background-index=false".into(),
                "--clang-tidy=false".into(),
            ],
        );
        app.lsp_manager.shutdown_all();
    }
}

#[test]
#[ignore = "requires installed pyright-langserver"]
fn real_format_document_pyright_shows_capability_error_without_breaking_lsp() {
    let (_dir, mut app) = real_app_format("python", "pyright-langserver", vec!["--stdio".into()]);
    assert!(render(&mut app, 100, 30).contains("Formatting failed"));
    app.lsp_manager.shutdown_all();
}
