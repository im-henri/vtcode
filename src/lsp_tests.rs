
use super::*;
use std::path::PathBuf;

/// Create a pending_requests map with a single entry for testing response parsing.
fn test_pending(id: i64, method: &str) -> Arc<Mutex<HashMap<i64, PendingRequest>>> {
    let map = HashMap::from([(id, method.into())]);
    Arc::new(Mutex::new(map))
}

/// Create an empty pending_requests map for testing notifications/errors.
fn test_empty_pending() -> Arc<Mutex<HashMap<i64, PendingRequest>>> {
    Arc::new(Mutex::new(HashMap::new()))
}

#[test]
fn semantic_tokens_decode_relative_positions() {
    let tokens = vec![
        SemanticToken {
            delta_line: 2,
            delta_start: 4,
            length: 3,
            token_type: 1,
            token_modifiers_bitset: 0,
        },
        SemanticToken {
            delta_line: 0,
            delta_start: 5,
            length: 2,
            token_type: 2,
            token_modifiers_bitset: 1,
        },
        SemanticToken {
            delta_line: 3,
            delta_start: 1,
            length: 4,
            token_type: 3,
            token_modifiers_bitset: 0,
        },
    ];

    let decoded = decode_semantic_tokens(&tokens);
    assert_eq!(
        decoded,
        vec![
            DecodedSemanticToken {
                line: 2,
                start_char: 4,
                length: 3,
                token_type: 1,
                _token_modifiers: 0,
            },
            DecodedSemanticToken {
                line: 2,
                start_char: 9,
                length: 2,
                token_type: 2,
                _token_modifiers: 1,
            },
            DecodedSemanticToken {
                line: 5,
                start_char: 1,
                length: 4,
                token_type: 3,
                _token_modifiers: 0,
            },
        ]
    );
}

#[test]
fn semantic_token_decode_can_cancel_obsolete_work() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let tokens = vec![
        SemanticToken {
            delta_line: 0,
            delta_start: 1,
            length: 1,
            token_type: 0,
            token_modifiers_bitset: 0,
        };
        2_048
    ];
    let checks = AtomicUsize::new(0);

    let decoded = decode_semantic_tokens_cancellable(&tokens, || {
        checks.fetch_add(1, Ordering::Relaxed) >= 1
    });

    assert!(decoded.is_none());
    assert_eq!(checks.load(Ordering::Relaxed), 2);
}

// ── LspServerConfig tests ──────────────────────────────────────

#[test]
fn test_lsp_server_config_deserialize() {
    let json = r#"{
            "language": "rust",
            "command": "rust-analyzer",
            "args": ["--log-file", "/tmp/ra.log"],
            "file_patterns": ["*.rs", "Cargo.toml"],
            "init_options": {"checkOnSave": true}
        }"#;
    let config: LspServerConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.language, "rust");
    assert_eq!(config.command, "rust-analyzer");
    assert_eq!(config.args, vec!["--log-file", "/tmp/ra.log"]);
    assert_eq!(config.file_patterns, vec!["*.rs", "Cargo.toml"]);
    assert!(config.init_options.is_some());
}

#[test]
fn test_lsp_server_config_defaults() {
    let json = r#"{"language": "test", "command": "test-server"}"#;
    let config: LspServerConfig = serde_json::from_str(json).unwrap();
    assert!(config.args.is_empty());
    assert!(config.file_patterns.is_empty());
    assert!(config.init_options.is_none());
}

// ── LspManager tests ──────────────────────────────────────────

#[test]
fn unconfigured_manager_requests_and_shutdown_leave_no_state() {
    let mut manager = LspManager::new(PathBuf::from("/tmp"));
    let path = Path::new("test.rs");
    assert!(manager.ensure_client_for_file(path).is_none());
    assert!(manager.active_server_names().is_empty());
    assert!(manager.get_diagnostics(path).is_none());
    assert!(!manager.has_diagnostics());
    manager.request_completion(path, 0, 0, "");
    manager.request_hover(path, 0, 0);
    manager.request_definition(path, 0, 0);
    assert!(manager.active_completion_id.is_none() && manager.completion_items.is_empty());
    assert!(manager.active_hover_id.is_none() && manager.hover_content.is_none());
    assert!(manager.active_definition_id.is_none() && manager.definition_locations.is_empty());
    manager.shutdown_all();
    assert!(manager.clients.is_empty());
}

#[test]
fn register_configs_keeps_first_server_per_language() {
    let mut manager = LspManager::new(PathBuf::from("/tmp"));
    let rust = LspServerConfig {
        language: "rust".into(),
        command: "rust-analyzer".into(),
        args: vec![],
        file_patterns: vec!["*.rs".into()],
        settings: None, init_options: None,
    };
    manager.register_config(rust.clone());
    assert_eq!(manager.configs.len(), 1);
    manager.register_config(LspServerConfig {
        command: "other-rust-server".into(),
        ..rust
    });
    assert_eq!(manager.configs.len(), 1);
    assert_eq!(manager.configs[0].command, "rust-analyzer");
    manager.register_config(LspServerConfig {
        language: "python".into(),
        command: "pylsp".into(),
        args: vec![],
        file_patterns: vec!["*.py".into()],
        settings: None, init_options: None,
    });
    assert_eq!(
        manager
            .configs
            .iter()
            .map(|config| config.language.as_str())
            .collect::<Vec<_>>(),
        ["rust", "python"]
    );
}

#[test]
fn test_get_diagnostics() {
    let mut mgr = LspManager::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("test.rs");
    mgr.diagnostics.insert(
        path.clone(),
        vec![LspDiagnostic {
            line: 5,
            col: 10,
            end_line: 5,
            end_col: 20,
            message: "unused variable".to_string(),
            severity: DiagnosticSeverity::WARNING,
        }],
    );
    assert!(mgr.has_diagnostics());
    let diags = mgr.get_diagnostics(&path).unwrap();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].line, 5);
    assert_eq!(diags[0].col, 10);
    assert_eq!(diags[0].message, "unused variable");
}

#[test]
fn test_notify_if_changed_version_tracking() {
    let mut mgr = LspManager::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("test.rs");
    // No client, so did_change won't actually send, but version tracking should work
    mgr.notify_if_changed(&path, 1, "content");
    assert_eq!(mgr.last_notified_version.get(&path), Some(&1));
    // Same version should not update
    mgr.notify_if_changed(&path, 1, "content");
    assert_eq!(mgr.last_notified_version.get(&path), Some(&1));
    // Higher version should update
    mgr.notify_if_changed(&path, 3, "content v3");
    assert_eq!(mgr.last_notified_version.get(&path), Some(&3));
}

#[test]
fn editing_document_retains_published_diagnostics_until_refresh() {
    let mut mgr = LspManager::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("test.cpp");
    mgr.diagnostics.insert(path.clone(), Vec::new());

    mgr.invalidate_document_feedback(&path);

    assert!(mgr.get_diagnostics(&path).is_some());
    assert!(mgr.diagnostic_dirty_documents.contains(&path));
    assert_eq!(mgr.diagnostics_version, 0);
}

// ── JSON-RPC message parsing tests ──────────────────────────────

#[test]
fn test_parse_diagnostics_notification() {
    let json = r#"{
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///test.rs",
                "version": 7,
                "diagnostics": [
                    {
                        "range": {
                            "start": {"line": 3, "character": 5},
                            "end": {"line": 3, "character": 15}
                        },
                        "severity": 1,
                        "message": "expected `;`"
                    }
                ]
            }
        }"#;
    let event = LspClient::parse_message(json, &test_empty_pending()).unwrap();
    match event {
        LspEvent::Diagnostics {
            uri,
            version,
            diagnostics,
        } => {
            assert_eq!(uri.path(), "/test.rs");
            assert_eq!(version, Some(7));
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].message, "expected `;`");
            assert_eq!(diagnostics[0].range.start.line, 3);
            assert_eq!(diagnostics[0].range.start.character, 5);
            assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        }
        _ => panic!("Expected Diagnostics event"),
    }
}

#[test]
fn test_parse_completion_response_array() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 5,
            "result": [
                {"label": "println!", "kind": 3, "detail": "macro"},
                {"label": "print!", "kind": 3}
            ]
        }"#;
    let event =
        LspClient::parse_message(json, &test_pending(5, "textDocument/completion")).unwrap();
    match event {
        LspEvent::CompletionResponse { request_id, items } => {
            assert_eq!(request_id, 5);
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].label, "println!");
            assert_eq!(items[1].label, "print!");
        }
        _ => panic!("Expected CompletionResponse event"),
    }
}

#[test]
fn test_parse_completion_response_list() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 7,
            "result": {
                "isIncomplete": false,
                "items": [
                    {"label": "Vec", "kind": 22},
                    {"label": "String", "kind": 22}
                ]
            }
        }"#;
    let event =
        LspClient::parse_message(json, &test_pending(7, "textDocument/completion")).unwrap();
    match event {
        LspEvent::CompletionResponse { request_id, items } => {
            assert_eq!(request_id, 7);
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].label, "Vec");
        }
        _ => panic!("Expected CompletionResponse event"),
    }
}

#[test]
fn test_parse_formatting_response_null() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 8,
            "result": null
        }"#;
    let event =
        LspClient::parse_message(json, &test_pending(8, "textDocument/formatting")).unwrap();
    match event {
        LspEvent::FormatResponse { request_id, edits } => {
            assert_eq!(request_id, 8);
            assert!(edits.is_empty());
        }
        _ => panic!("Expected FormatResponse event"),
    }
}

#[test]
fn test_parse_range_formatting_response_null() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 9,
            "result": null
        }"#;
    let event =
        LspClient::parse_message(json, &test_pending(9, "textDocument/rangeFormatting")).unwrap();
    match event {
        LspEvent::FormatResponse { request_id, edits } => {
            assert_eq!(request_id, 9);
            assert!(edits.is_empty());
        }
        _ => panic!("Expected FormatResponse event"),
    }
}

#[test]
fn test_parse_hover_response() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 10,
            "result": {
                "contents": {"kind": "plaintext", "value": "fn main()"}
            }
        }"#;
    let event = LspClient::parse_message(json, &test_pending(10, "textDocument/hover")).unwrap();
    match event {
        LspEvent::HoverResponse {
            request_id,
            contents,
        } => {
            assert_eq!(request_id, 10);
            assert_eq!(contents, Some("fn main()".to_string()));
        }
        _ => panic!("Expected HoverResponse event"),
    }
}

#[test]
fn test_parse_definition_response_scalar() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 15,
            "result": {
                "uri": "file:///src/lib.rs",
                "range": {
                    "start": {"line": 10, "character": 0},
                    "end": {"line": 10, "character": 20}
                }
            }
        }"#;
    let event =
        LspClient::parse_message(json, &test_pending(15, "textDocument/definition")).unwrap();
    match event {
        LspEvent::DefinitionResponse {
            request_id,
            locations,
        } => {
            assert_eq!(request_id, 15);
            assert_eq!(locations.len(), 1);
            assert_eq!(locations[0].range.start.line, 10);
        }
        _ => panic!("Expected DefinitionResponse event"),
    }
}

#[test]
fn test_parse_definition_response_array() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 16,
            "result": [
                {
                    "uri": "file:///src/a.rs",
                    "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 5}}
                },
                {
                    "uri": "file:///src/b.rs",
                    "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 5}}
                }
            ]
        }"#;
    let event =
        LspClient::parse_message(json, &test_pending(16, "textDocument/definition")).unwrap();
    match event {
        LspEvent::DefinitionResponse {
            request_id,
            locations,
        } => {
            assert_eq!(request_id, 16);
            assert_eq!(locations.len(), 2);
        }
        _ => panic!("Expected DefinitionResponse event"),
    }
}

#[test]
fn test_parse_error_response() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 20,
            "error": {
                "code": -32600,
                "message": "Invalid Request"
            }
        }"#;
    let event = LspClient::parse_message(json, &test_pending(20, "")).unwrap();
    match event {
        LspEvent::RequestError { request_id: 20, .. } => {
            // The transport remains usable after a request error.
        }
        _ => panic!("Expected RequestError event"),
    }
}

#[test]
fn test_parse_unknown_notification() {
    let json = r#"{
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": {"type": 3, "message": "Loading..."}
        }"#;
    assert!(LspClient::parse_message(json, &test_empty_pending()).is_none());
}

#[test]
fn test_parse_invalid_json() {
    assert!(LspClient::parse_message("not json at all", &test_empty_pending()).is_none());
}

#[test]
fn test_parse_null_result() {
    // e.g. hover returns null when no info is available
    let json = r#"{"jsonrpc": "2.0", "id": 30, "result": null}"#;
    // Should return None since null doesn't match any expected response type
    assert!(LspClient::parse_message(json, &test_pending(30, "unknown")).is_none());
}

#[test]
fn test_parameter_label_offsets_short_input_is_safe() {
    assert_eq!(LspClient::parameter_label_offsets(&[]), (0, 0));
    assert_eq!(LspClient::parameter_label_offsets(&[4]), (4, 4));
    assert_eq!(LspClient::parameter_label_offsets(&[4, 9]), (4, 9));
}

#[test]
fn test_parse_document_symbol_nested_response_without_panicking() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 32,
            "result": [
                {
                    "name": "main",
                    "kind": 12,
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 4, "character": 1}
                    },
                    "selectionRange": {
                        "start": {"line": 0, "character": 3},
                        "end": {"line": 0, "character": 7}
                    }
                }
            ]
        }"#;
    let event =
        LspClient::parse_message(json, &test_pending(32, "textDocument/documentSymbol")).unwrap();
    match event {
        LspEvent::DocumentSymbolResponse {
            request_id,
            symbols,
        } => {
            assert_eq!(request_id, 32);
            assert_eq!(symbols.len(), 1);
            assert_eq!(symbols[0].name, "main");
        }
        _ => panic!("Expected DocumentSymbolResponse event"),
    }
}

// ── File pattern matching tests ──────────────────────────────

#[test]
fn configured_file_patterns_match_extensions_and_exact_basenames_without_starting_servers() {
    let mut manager = LspManager::new(PathBuf::from("/tmp"));
    manager.register_config(LspServerConfig {
        language: "rust".into(),
        command: "unused-server".into(),
        args: vec![],
        file_patterns: vec!["*.rs".into(), "Cargo.toml".into()],
        settings: None, init_options: None,
    });
    for (path, expected) in [
        ("src/main.rs", true),
        ("project/Cargo.toml", true),
        ("src/main.py", false),
        ("Cargo.toml.backup", false),
        ("src/main.RS", false),
    ] {
        assert_eq!(
            manager.has_config_for_file(Path::new(path)),
            expected,
            "{path}"
        );
    }
    assert!(manager.clients.is_empty());
}

// ── Severity helper tests ──────────────────────────────────────

#[test]
fn test_severity_icon() {
    assert_eq!(severity_icon(DiagnosticSeverity::ERROR), "✗");
    assert_eq!(severity_icon(DiagnosticSeverity::WARNING), "⚠");
    assert_eq!(severity_icon(DiagnosticSeverity::INFORMATION), "ℹ");
    assert_eq!(severity_icon(DiagnosticSeverity::HINT), "·");
}

#[test]
fn test_severity_color() {
    use ratatui::style::Color;
    assert_eq!(
        severity_color(DiagnosticSeverity::ERROR),
        Color::Rgb(255, 80, 80)
    );
    assert_eq!(
        severity_color(DiagnosticSeverity::WARNING),
        Color::Rgb(255, 200, 50)
    );
    assert_eq!(
        severity_color(DiagnosticSeverity::INFORMATION),
        Color::Rgb(100, 180, 255)
    );
    assert_eq!(
        severity_color(DiagnosticSeverity::HINT),
        Color::Rgb(150, 150, 150)
    );
}

// ── markup_content_to_string tests ────────────────────────────

#[test]
fn test_markup_string_conversion() {
    let s = markup_content_to_string(MarkedString::String("hello".to_string()));
    assert_eq!(s, "hello");
}

#[test]
fn test_markup_language_string_conversion() {
    let s = markup_content_to_string(MarkedString::LanguageString(LanguageString {
        language: "rust".to_string(),
        value: "fn main() {}".to_string(),
    }));
    assert_eq!(s, "fn main() {}");
}

// ── JSON-RPC framing tests ───────────────────────────────────

#[test]
fn test_jsonrpc_request_serialization() {
    let req = JsonRpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "textDocument/hover".to_string(),
        params: Some(serde_json::json!({"textDocument": {"uri": "file:///test.rs"}})),
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"jsonrpc\":\"2.0\""));
    assert!(json.contains("\"id\":1"));
    assert!(json.contains("\"method\":\"textDocument/hover\""));
}

#[test]
fn test_jsonrpc_request_no_params() {
    let req = JsonRpcRequest {
        jsonrpc: "2.0",
        id: 2,
        method: "shutdown".to_string(),
        params: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    // params should be skipped due to skip_serializing_if
    assert!(!json.contains("params"));
}

#[test]
fn test_jsonrpc_notification_serialization() {
    let notif = JsonRpcNotification {
        jsonrpc: "2.0",
        method: "initialized".to_string(),
        params: Some(serde_json::json!({})),
    };
    let json = serde_json::to_string(&notif).unwrap();
    assert!(json.contains("\"method\":\"initialized\""));
    assert!(!json.contains("\"id\""));
}

// ── Integration test: rust-analyzer lifecycle ────────────────

#[test]
fn test_rust_analyzer_start_and_initialize() {
    // Skip if rust-analyzer is not available
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("Skipping: rust-analyzer not found");
        return;
    }

    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path();
    std::fs::create_dir(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname='lsp-lifecycle-fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .unwrap();
    let test_path = workspace.join("src/main.rs");
    let content = "fn main() {}\n";
    std::fs::write(&test_path, content).unwrap();
    let config = LspServerConfig {
        language: "rust".to_string(),
        command: "rust-analyzer".to_string(),
        args: vec![],
        file_patterns: vec!["*.rs".to_string()],
        settings: None, init_options: None,
    };

    let mut client = LspClient::start(&config, workspace).unwrap();
    assert_eq!(client.language, "rust");
    assert!(!client.initialized);

    // Wait for initialize response
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        match client
            .event_rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
        {
            Ok(LspEvent::Initialized { capabilities: _ }) => {
                client.initialized = true;
                let _ = client.send_initialized(); // Error OK: LSP notification is best-effort, server may be down
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(
        client.initialized,
        "rust-analyzer should initialize within 10s"
    );

    // Test did_open
    assert!(client.did_open(&test_path, "rust", content).is_ok());
    assert!(client.is_file_open(&test_path));

    // Test did_close
    assert!(client.did_close(&test_path).is_ok());
    assert!(!client.is_file_open(&test_path));

    // Clean shutdown
    assert!(client.shutdown().is_ok());
}

#[test]
fn test_lsp_manager_ensure_client_for_file() {
    // Skip if rust-analyzer is not available
    if std::process::Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("Skipping: rust-analyzer not found");
        return;
    }

    let temporary = tempfile::tempdir().unwrap();
    let mut mgr = LspManager::new(temporary.path().to_path_buf());
    mgr.register_config(LspServerConfig {
        language: "rust".to_string(),
        command: "rust-analyzer".to_string(),
        args: vec![],
        file_patterns: vec!["*.rs".to_string()],
        settings: None, init_options: None,
    });

    // Should start a client for .rs files
    let idx = mgr.ensure_client_for_file(Path::new("test.rs"));
    assert!(idx.is_some());
    assert_eq!(mgr.clients.len(), 1);
    assert_eq!(mgr.active_server_names(), vec!["rust"]);

    // Should return same client index for another .rs file
    let idx2 = mgr.ensure_client_for_file(Path::new("other.rs"));
    assert_eq!(idx, idx2);
    assert_eq!(mgr.clients.len(), 1);

    // Should return None for unmatched file types
    let idx3 = mgr.ensure_client_for_file(Path::new("test.py"));
    assert!(idx3.is_none());

    // Clean up
    mgr.shutdown_all();
}

#[test]
fn test_ensure_client_invalid_command() {
    let mut mgr = LspManager::new(PathBuf::from("/tmp"));
    mgr.register_config(LspServerConfig {
        language: "fake".to_string(),
        command: "nonexistent-lsp-server-binary".to_string(),
        args: vec![],
        file_patterns: vec!["*.fake".to_string()],
        settings: None, init_options: None,
    });
    let idx = mgr.ensure_client_for_file(Path::new("test.fake"));
    assert!(idx.is_none()); // Should fail to start
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires rust-analyzer, rust-src and rustfmt"]
fn rust_analyzer_linux_feature_round_trip() {
    use std::time::{Duration, Instant};

    fn wait(manager: &mut LspManager, label: &str, ready: impl Fn(&LspManager) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            manager.poll();
            if ready(manager) { break; }
            assert!(Instant::now() < deadline, "language server timed out: {label}");
            assert!(!manager.clients.is_empty(), "language server disconnected: {label}");
            std::thread::sleep(Duration::from_millis(20));
        }
        eprintln!("rust-analyzer Linux: {label}");
    }

    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("workspace with spaces 世界");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname='linux-lsp-probe'\nversion='0.1.0'\nedition='2024'\n").unwrap();
    let path = root.join("src/lib.rs");
    let source = "pub fn add(left:i32,right:i32)->i32{\nleft+right\n}\npub fn caller()->i32{\n    let result=add(1,2);\n    result\n}\npub fn invalid(){ let trouble:i32=\"oops\"; }\n";
    std::fs::write(&path, source).unwrap();
    let mut manager = LspManager::new(root);
    manager.register_config(LspServerConfig {
        language: "rust".into(), command: "rust-analyzer".into(), args: vec![],
        file_patterns: vec!["*.rs".into()], settings: None, init_options: None,
    });
    assert!(manager.ensure_client_for_file(&path).is_some());
    wait(&mut manager, "initialize", |m| m.clients[0].initialized);
    manager.sync_document_owned(&path, 1, source.to_string());
    wait(&mut manager, "diagnostics", |m| m.get_diagnostics(&path).is_some_and(|diagnostics| diagnostics.iter().any(|d| d.severity == DiagnosticSeverity::ERROR)));

    // Diagnostics can arrive while rust-analyzer is still indexing. Repeat
    // the completion probe until the workspace is ready, as a user would.
    let completion_deadline = Instant::now() + Duration::from_secs(45);
    while manager.completion_items.is_empty() {
        assert!(manager.request_completion(&path, 4, 17, "ad"));
        let attempt_deadline = Instant::now() + Duration::from_millis(500);
        while manager.completion_items.is_empty() && Instant::now() < attempt_deadline {
            manager.poll();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(Instant::now() < completion_deadline, "workspace never became ready for completion");
    }
    wait(&mut manager, "completion", |m| !m.completion_items.is_empty());
    assert!(manager.completion_items.iter().any(|item| item.label.starts_with("add")), "{:?}", manager.completion_items);
    assert!(manager.request_hover(&path, 4, 16));
    wait(&mut manager, "hover", |m| m.hover_content.is_some());
    assert!(manager.hover_content.as_ref().is_some_and(|text| text.contains("add")));
    assert!(manager.request_definition(&path, 4, 16));
    wait(&mut manager, "definition", |m| !m.definition_locations.is_empty());
    assert!(manager.definition_locations.iter().any(|location| location.range.start.line == 0 && location.uri.to_file_path().ok().as_ref() == Some(&path)));
    assert!(manager.request_references(&path, 0, 8));
    wait(&mut manager, "references", |m| !m.reference_locations.is_empty());
    assert!(manager.reference_locations.len() >= 2);
    assert!(manager.request_rename(&path, 0, 8, "sum"));
    wait(&mut manager, "rename", |m| m.rename_edit.is_some());
    assert!(serde_json::to_string(&manager.rename_edit).unwrap().contains("sum"));
    assert!(manager.request_format_document(&path, 4, true));
    wait(&mut manager, "formatting", |m| m.format_dirty);
    assert!(!manager.format_edits.is_empty());
    assert!(manager.request_code_action(&CodeActionParams {
        path: &path, start_line: 1, start_col: 0, end_line: 1, end_col: 10,
        diagnostics: vec![],
    }));
    wait(&mut manager, "code actions", |m| !m.code_actions.is_empty());
    assert!(manager.code_actions.iter().any(|action| action.edit.is_some()));
    assert!(manager.request_signature_help(&path, 4, 21));
    wait(&mut manager, "signature help", |m| m.signature_help.is_some());
    assert!(manager.signature_help.is_some());
    assert!(manager.request_document_symbol(&path));
    wait(&mut manager, "outline", |m| m.document_symbols_dirty);
    assert!(manager.document_symbols.iter().any(|symbol| symbol.name == "caller"));
    assert!(manager.request_semantic_tokens_full(&path));
    wait(&mut manager, "semantic tokens", |m| m.semantic_tokens_dirty);
    assert!(manager.semantic_tokens_response.as_ref().is_some_and(|tokens| !tokens.data.is_empty()));
    assert!(manager.request_folding_range(&path));
    wait(&mut manager, "folding ranges", |m| m.folding_ranges_dirty);
    assert!(!manager.folding_ranges.is_empty());
    assert!(manager.request_inlay_hints(&path, 0, 8));
    wait(&mut manager, "inlay hints", |m| m.inlay_hints_dirty);
    assert!(!manager.inlay_hints.is_empty());

    let fixed = source.replace("\"oops\"", "42");
    std::fs::write(&path, &fixed).unwrap();
    manager.did_change(&path, &fixed);
    manager.notify_did_save(&path);
    wait(&mut manager, "compiler diagnostics after save", |m| m.get_diagnostics(&path).is_some_and(|diagnostics| diagnostics.iter().all(|d| d.severity != DiagnosticSeverity::ERROR)));
    // Cargo-check diagnostics update on save. Parser diagnostics must also
    // update for unsaved text without changing the file on disk.
    let broken = format!("{fixed}\nfn syntax_error() {{ let broken = ; }}\n");
    manager.did_change(&path, &broken);
    wait(&mut manager, "diagnostics for unsaved syntax error", |m| m.get_diagnostics(&path).is_some_and(|diagnostics| diagnostics.iter().any(|d| d.line >= 8 && d.severity == DiagnosticSeverity::ERROR)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), fixed);
    let previous_diagnostics = manager.diagnostics_version;
    manager.did_change(&path, &fixed);
    wait(&mut manager, "diagnostics after unsaved edit", |m| m.diagnostics_version > previous_diagnostics && m.get_diagnostics(&path).is_none_or(|diagnostics| diagnostics.iter().all(|d| d.severity != DiagnosticSeverity::ERROR)));
    manager.did_close(&path);
    assert!(manager.get_diagnostics(&path).is_none());
    manager.shutdown_all();
    assert!(manager.clients.is_empty());
}

#[test]
fn test_parse_initialize_result() {
    let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": {
                    "textDocumentSync": 1,
                    "completionProvider": {"triggerCharacters": ["."]},
                    "hoverProvider": true,
                    "definitionProvider": true
                }
            }
        }"#;
    let event = LspClient::parse_message(json, &test_pending(1, "initialize")).unwrap();
    match event {
        LspEvent::Initialized { capabilities: _ } => {} // Expected
        _ => panic!("Expected Initialized event"),
    }
}

#[test]
fn test_parse_diagnostics_multiple() {
    let json = r#"{
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///src/main.rs",
                "diagnostics": [
                    {
                        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}},
                        "severity": 1,
                        "message": "error1"
                    },
                    {
                        "range": {"start": {"line": 2, "character": 3}, "end": {"line": 2, "character": 10}},
                        "severity": 2,
                        "message": "warning1"
                    },
                    {
                        "range": {"start": {"line": 5, "character": 0}, "end": {"line": 7, "character": 1}},
                        "severity": 3,
                        "message": "info1"
                    }
                ]
            }
        }"#;
    let event = LspClient::parse_message(json, &test_empty_pending()).unwrap();
    match event {
        LspEvent::Diagnostics { diagnostics, .. } => {
            assert_eq!(diagnostics.len(), 3);
            assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
            assert_eq!(diagnostics[1].severity, Some(DiagnosticSeverity::WARNING));
            assert_eq!(
                diagnostics[2].severity,
                Some(DiagnosticSeverity::INFORMATION)
            );
        }
        _ => panic!("Expected Diagnostics event"),
    }
}
