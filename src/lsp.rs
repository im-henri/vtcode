//! Language Server Protocol client implementation.
//!
//! Provides JSON-RPC over stdin/stdout transport for communicating with
//! LSP-compatible language servers. Each `LspClient` manages one server process.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
#[cfg(test)]
use std::process::Command;
use std::sync::{mpsc, Arc, Mutex};

use lsp_types::Url;
use lsp_types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ropey::Rope;

#[path = "lsp_transport.rs"]
mod transport;
#[path = "lsp_outbound.rs"]
mod outbound;
#[path = "lsp_completion.rs"]
pub(crate) mod completion;
#[path = "lsp_diagnostics.rs"]
pub(crate) mod diagnostics;

#[cfg(test)]
#[path = "lsp_performance_tests.rs"]
mod performance_tests;

/// A stdio language server supplied by settings or an extension manifest.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LspServerConfig {
    /// Language identifier (e.g. "rust", "python", "typescript")
    pub language: String,
    /// Command to start the language server
    pub command: String,
    /// Arguments for the command
    #[serde(default)]
    pub args: Vec<String>,
    /// File patterns this server handles (e.g. ["*.rs", "*.toml"])
    #[serde(default, alias = "file_patterns")]
    pub file_patterns: Vec<String>,
    /// Additional initialization options
    #[serde(default, alias = "init_options")]
    pub init_options: Option<Value>,
    /// Server configuration, such as python.analysis and python.pythonPath.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
}

impl LspServerConfig {
    pub(crate) fn handles_file(&self, path: &Path) -> bool {
        matches_file_patterns(&self.file_patterns, path)
    }
}

fn matches_file_patterns(patterns: &[String], path: &Path) -> bool {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    patterns.iter().any(|pattern| {
        if pattern.starts_with("*.") { name.ends_with(&pattern[1..]) } else { name == pattern }
    })
}

// LspEvent enum is in lsp_events.rs
pub(crate) use crate::lsp_events::LspEvent;

/// A decoded semantic token with absolute line/col and type/modifier info
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedSemanticToken {
    pub line: u32,
    pub start_char: u32,
    pub length: u32,
    pub token_type: u32,
    pub _token_modifiers: u32,
}

/// Decode LSP semantic tokens from relative encoding to absolute positions
#[cfg(test)]
pub(crate) fn decode_semantic_tokens(data: &[SemanticToken]) -> Vec<DecodedSemanticToken> {
    decode_semantic_tokens_cancellable(data, || false).unwrap_or_default()
}

/// Decode a full semantic-token stream on a worker while allowing a newer
/// document version to cancel obsolete work.
pub(crate) fn decode_semantic_tokens_cancellable(
    data: &[SemanticToken],
    is_cancelled: impl Fn() -> bool,
) -> Option<Vec<DecodedSemanticToken>> {
    let mut result = Vec::with_capacity(data.len());
    let mut line: u32 = 0;
    let mut char_offset: u32 = 0;
    for (index, token) in data.iter().enumerate() {
        if index % 1_024 == 0 && is_cancelled() {
            return None;
        }
        if token.delta_line > 0 {
            line += token.delta_line;
            char_offset = token.delta_start;
        } else {
            char_offset += token.delta_start;
        }
        result.push(DecodedSemanticToken {
            line,
            start_char: char_offset,
            length: token.length,
            token_type: token.token_type,
            _token_modifiers: token.token_modifiers_bitset,
        });
    }
    (!is_cancelled()).then_some(result)
}

/// Resolve the active theme's complete semantic style, including scope fallbacks.
pub(crate) fn themed_semantic_token_style(token: &DecodedSemanticToken, legend: &SemanticTokensLegend, language: &str, theme: Option<&crate::semantic_theme::SemanticTheme>) -> Option<ratatui::style::Style> {
    theme?.style(token, legend, language)
}
/// JSON-RPC request
#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: i64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

/// JSON-RPC notification (no id)
#[derive(Serialize)]
struct JsonRpcNotification {
    jsonrpc: &'static str,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

enum LspOutbound {
    Json(String),
    SyncKind(TextDocumentSyncKind),
    DidOpen {
        uri: String,
        language_id: String,
        version: i32,
        text: Rope,
    },
    DidChange {
        uri: String,
        version: i32,
        text: Rope,
    },
    DidClose { uri: Url },
}

/// Reader-side work must use the context captured before a request is sent,
/// rather than the UI's prefix when a possibly delayed reply arrives.
pub(crate) struct PendingRequest {
    pub(crate) method: String,
    pub(crate) completion_prefix: Option<String>,
}

impl From<&str> for PendingRequest {
    fn from(method: &str) -> Self {
        Self { method: method.into(), completion_prefix: None }
    }
}

/// The exact snapshot and server behind one passive document request. The
/// key remains after completion (including empty/error responses), so idle
/// polling never repeats the same analysis.
#[derive(Clone)]
pub(crate) struct DocumentFeedbackRequest {
    pub(crate) target: crate::background::LspDocumentTarget,
    pub(crate) range: Option<(u32, u32)>,
    pub(crate) client_idx: usize,
}

impl DocumentFeedbackRequest {
    pub(crate) fn matches(&self, path: &Path, version: u64, range: Option<(u32, u32)>) -> bool {
        self.target.path == path && self.target.content_version == version && self.range == range
    }
}

pub(crate) struct FormatRangeParams<'a> {
    pub(crate) path: &'a Path,
    pub(crate) start_line: u32,
    pub(crate) start_col: u32,
    pub(crate) end_line: u32,
    pub(crate) end_col: u32,
    pub(crate) tab_size: u32,
    pub(crate) insert_spaces: bool,
}

pub(crate) struct CodeActionParams<'a> {
    pub(crate) path: &'a Path,
    pub(crate) start_line: u32,
    pub(crate) start_col: u32,
    pub(crate) end_line: u32,
    pub(crate) end_col: u32,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

pub(crate) struct OnTypeFormattingParams<'a> {
    pub(crate) path: &'a Path,
    pub(crate) line: u32,
    pub(crate) col: u32,
    pub(crate) ch: &'a str,
    pub(crate) tab_size: u32,
    pub(crate) insert_spaces: bool,
}

/// LSP client managing a single language server process.
pub(crate) struct LspClient {
    /// Language this client serves
    pub language: String,
    /// File patterns
    pub file_patterns: Vec<String>,
    pub(crate) file_associations: Arc<crate::file_associations::Associations>,
    /// Server process
    pub(crate) child: crate::remote::lsp::LspProcess,
    /// Outbound messages are serialized and written by a dedicated thread so
    /// a full pipe or a large document notification cannot block the UI.
    outbound_tx: outbound::Sender,
    /// Channel for receiving events from the reader thread
    pub event_rx: mpsc::Receiver<LspEvent>,
    /// Tracks which files have been opened with didOpen
    open_files: std::collections::HashSet<PathBuf>,
    /// Whether initialization is complete
    pub initialized: bool,
    /// Server capabilities are per process, including in mixed-language workspaces.
    pub(crate) capabilities: Option<ServerCapabilities>,
    supported_requests: Option<HashSet<&'static str>>,
    /// Keep only the latest snapshot while the initialize handshake is pending.
    pending_opens: HashMap<PathBuf, LspOutbound>,
    pub(crate) settings: Value,
    /// Set by the reader/writer lifecycle event. This replaces querying every
    /// child process on each UI pass just to discover whether it exited.
    pub(crate) server_dead: bool,
    /// Maps request IDs to their method names for response disambiguation
    pending_requests: Arc<Mutex<HashMap<i64, PendingRequest>>>,
}

/// In-memory server endpoint exercising the real request serialization,
/// pending-method parser and manager dispatch without an installed server.
#[cfg(test)]
pub(crate) struct TestLspServer {
    outbound_rx: mpsc::Receiver<LspOutbound>,
    event_tx: mpsc::Sender<LspEvent>,
    pending_requests: Arc<Mutex<HashMap<i64, PendingRequest>>>,
    writer: std::cell::RefCell<transport::LspWriter>,
}

#[cfg(test)]
impl TestLspServer {
    pub(crate) fn take_messages(&self) -> Vec<Value> {
        let mut buffer = Vec::new();
        let mut writer = self.writer.borrow_mut();
        self.outbound_rx.try_iter().filter_map(|message| {
            writer.encode(message, &mut buffer).unwrap().then(|| serde_json::from_slice(&buffer).unwrap())
        }).collect()
    }

    pub(crate) fn respond(&self, response: Value) {
        if let Some(event) = LspClient::parse_message(&response.to_string(), &self.pending_requests)
        {
            self.event_tx.send(event).unwrap();
        }
    }

    pub(crate) fn queue_event(&self, event: LspEvent) {
        self.event_tx.send(event).unwrap();
    }
}

impl LspClient {
    #[cfg(test)]
    pub(crate) fn test_pair(language: &str, patterns: &[&str]) -> (Self, TestLspServer) {
        // Keep the production Child type, but never start an external language
        // server. The already-exited test-list process makes cleanup immediate.
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        let (outbound_tx, outbound_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let pending_requests = Arc::new(Mutex::new(HashMap::new()));
        let server = TestLspServer {
            outbound_rx,
            event_tx,
            pending_requests: pending_requests.clone(),
            writer: Default::default(),
        };
        (
            Self {
                language: language.to_string(),
                file_patterns: patterns
                    .iter()
                    .map(|pattern| (*pattern).to_string())
                    .collect(),
                file_associations: Arc::default(),
                child: crate::remote::lsp::LspProcess::Local(child),
                outbound_tx: outbound::Sender::Local(outbound_tx),
                event_rx,
                open_files: HashSet::new(),
                initialized: true,
                capabilities: None,
                supported_requests: None,
                pending_opens: HashMap::new(),
                settings: Value::Null,
                server_dead: false,
                pending_requests,
            },
            server,
        )
    }

    /// Spawn the language server and start the reader thread.
    #[cfg(test)]
    pub(crate) fn start(config: &LspServerConfig, workspace_root: &Path) -> Result<Self, String> {
        Self::start_with_wake(config, workspace_root, crate::background::UiWake::default())
    }

    pub(crate) fn start_with_wake(
        config: &LspServerConfig,
        workspace_root: &Path,
        wake: crate::background::UiWake,
    ) -> Result<Self, String> {
        let mut command = vtcode::acp::local_command(&config.command, workspace_root, std::iter::empty());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        }
        let mut child = command.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start LSP server '{}': {}", config.command, e))?;

        // Coalesce header fragments and small JSON messages into one pipe write.
        let stdin = std::io::BufWriter::new(child.stdin.take().ok_or("Failed to get stdin")?);
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
        Self::start_transport(config, workspace_root, wake, crate::remote::lsp::LspProcess::Local(child), Box::new(stdin), Box::new(stdout), None)
    }

    pub(crate) fn start_remote(config: &LspServerConfig, workspace: &crate::remote::workspace::Workspace, wake: crate::background::UiWake) -> Result<Self, String> {
        let process = workspace.client.spawn_async(config.command.clone(), config.args.clone(), crate::remote::RemotePath::default(), None, wake.clone()).map_err(|error| error.to_string())?;
        let (stdout, stdin, child) = process.split();
        Self::start_transport(config, &workspace.view_root, wake, crate::remote::lsp::LspProcess::Remote(child), Box::new(std::io::BufWriter::new(stdin)), Box::new(stdout), Some(workspace.clone()))
    }

    pub(crate) fn start_transport(config: &LspServerConfig, workspace_root: &Path, wake: crate::background::UiWake,
        child: crate::remote::lsp::LspProcess, mut stdin: Box<dyn Write + Send>, stdout: Box<dyn std::io::Read + Send>,
        remote: Option<crate::remote::workspace::Workspace>) -> Result<Self, String> {

        let (event_tx, event_rx) = mpsc::channel();
        let pending_requests: Arc<Mutex<HashMap<i64, PendingRequest>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_for_reader = Arc::clone(&pending_requests);
        enum Receiver { Local(mpsc::Receiver<LspOutbound>), Remote(outbound::Receiver) }
        impl Receiver { fn recv(&self) -> Result<LspOutbound, mpsc::RecvError> {
            match self { Self::Local(receiver) => receiver.recv(), Self::Remote(receiver) => receiver.recv() }
        } }
        let (outbound_tx, outbound_rx) = if remote.is_some() {
            let (sender, receiver) = outbound::channel(); (sender, Receiver::Remote(receiver))
        } else {
            let (sender, receiver) = mpsc::channel(); (outbound::Sender::Local(sender), Receiver::Local(receiver))
        };
        let writer_event_tx = event_tx.clone();
        let writer_wake = wake.clone();
        let writer_remote = remote.clone();

        std::thread::Builder::new()
            .name(format!("lsp-writer-{}", config.language))
            .spawn(move || {
                let mut writer = transport::LspWriter::default();
                let mut buffer = Vec::new();
                while let Ok(mut message) = outbound_rx.recv() {
                    if let Some(remote) = &writer_remote {
                        match &mut message {
                            LspOutbound::DidOpen { uri, .. } | LspOutbound::DidChange { uri, .. } => {
                                if let Some(mapped) = remote.lsp_uri(uri, true) { *uri = mapped; }
                            }
                            LspOutbound::DidClose { uri } => {
                                if let Some(mapped) = remote.lsp_uri(uri.as_str(), true).and_then(|uri| Url::parse(&uri).ok()) { *uri = mapped; }
                            }
                            LspOutbound::Json(json) => {
                                if let Ok(mut value) = serde_json::from_str::<Value>(json) {
                                    remote.rewrite_lsp(&mut value, true);
                                    *json = value.to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                    let written = match writer.encode(message, &mut buffer) {
                        Ok(false) => continue,
                        Ok(true) => (|| -> std::io::Result<()> {
                            write!(stdin, "Content-Length: {}\r\n\r\n", buffer.len())?;
                            stdin.write_all(&buffer)?; stdin.flush()
                        })(),
                        Err(error) => Err(std::io::Error::other(error)),
                    };
                    if written.is_err() {
                        if writer_event_tx.send(LspEvent::ServerError(())).is_ok() { writer_wake.notify(); }
                        break;
                    }
                }
            })
            .map_err(|error| format!("Failed to spawn LSP writer thread: {error}"))?;

        // Spawn background reader thread
        std::thread::Builder::new()
            .name(format!("lsp-reader-{}", config.language))
            .spawn(move || {
                Self::reader_loop_mapped(stdout, event_tx, pending_for_reader, wake, remote);
            })
            .map_err(|e| format!("Failed to spawn reader thread: {}", e))?;

        let mut client = LspClient {
            language: config.language.clone(),
            file_patterns: config.file_patterns.clone(),
            file_associations: Arc::default(),
            child,
            outbound_tx,
            event_rx,
            open_files: std::collections::HashSet::new(),
            initialized: false,
            capabilities: None,
            supported_requests: None,
            pending_opens: HashMap::new(),
            settings: config.settings.clone().unwrap_or(Value::Null),
            server_dead: false,
            pending_requests,
        };

        // Send initialize request
        client.send_initialize(workspace_root, config.init_options.clone())?;

        Ok(client)
    }

    /// Send a raw JSON-RPC message with Content-Length framing
    fn send_raw(&mut self, json: &str) -> Result<(), String> {
        self.outbound_tx
            .send(LspOutbound::Json(json.to_string()))
            .map_err(|error| format!("LSP writer stopped: {error}"))
    }

    /// Send a JSON-RPC response to a server request
    pub(crate) fn send_response(&mut self, id: impl Into<Value>, result: Value) -> Result<(), String> {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.into(),
            "result": result,
        });
        let json =
            serde_json::to_string(&response).map_err(|e| format!("Serialize error: {}", e))?;
        self.send_raw(&json)
    }

    /// Whether this client owns an outstanding request.
    pub(crate) fn has_pending_request(&self, id: i64) -> bool {
        self.pending_requests
            .lock()
            .is_ok_and(|requests| requests.contains_key(&id))
    }

    /// Send a request (with id) and return the request id
    pub(crate) fn send_request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<i64, String> {
        self.send_request_with_context(method.into(), params)
    }

    pub(crate) fn send_request_with_context(
        &mut self,
        context: PendingRequest,
        params: Option<Value>,
    ) -> Result<i64, String> {
        let method = context.method.as_str();
        if method != "initialize" && method != "shutdown" && !self.initialized {
            return Err("Language server is still initializing".into());
        }
        if !self.supports_request(method) {
            return Err(format!("Language server does not support {method}"));
        }
        // IDs are unique across processes and restarts. A late Python reply
        // cannot satisfy a newer C++ request with the same per-server counter.
        static NEXT_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if matches!(method, "textDocument/completion" | "completionItem/resolve"
            | "textDocument/hover" | "textDocument/signatureHelp" | "textDocument/documentHighlight"
            | "textDocument/semanticTokens/full")
        {
            let obsolete: Vec<_> = self.pending_requests.lock().ok().map(|requests|
                requests.iter().filter_map(|(&id, pending)| (pending.method == method).then_some(id)).collect()
            ).unwrap_or_default();
            for obsolete in obsolete { self.cancel_request(obsolete); }
        }
        let req = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };
        let json = serde_json::to_string(&req).map_err(|e| format!("Serialize error: {}", e))?;
        if let Ok(mut map) = self.pending_requests.lock() {
            map.insert(id, context);
        }
        if let Err(error) = self.send_raw(&json) {
            if let Ok(mut requests) = self.pending_requests.lock() { requests.remove(&id); }
            return Err(error);
        }
        Ok(id)
    }

    /// Send a notification (no id, no response expected)
    fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<(), String> {
        let notif = JsonRpcNotification {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
        };
        let json = serde_json::to_string(&notif).map_err(|e| format!("Serialize error: {}", e))?;
        self.send_raw(&json)
    }

    /// Discard obsolete replies at the reader and ask the server to stop work.
    /// A reply may already be queued, so the manager also checks its active ID.
    fn cancel_request(&mut self, id: i64) {
        let pending = self
            .pending_requests
            .lock()
            .ok()
            .and_then(|mut requests| requests.remove(&id))
            .is_some();
        if pending {
            let _ = self.send_notification("$/cancelRequest", Some(serde_json::json!({"id": id})));
        }
    }

    // ── LSP Lifecycle ─────────────────────────────────────────────

    fn send_initialize(&mut self, root: &Path, init_options: Option<Value>) -> Result<i64, String> {
        let root_uri = Url::from_file_path(root).map_err(|_| "Invalid root path")?;
        // root_path is deprecated in the LSP spec (root_uri is preferred) but
        // InitializeParams still carries the field; suppress the warning.
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            client_info: Some(ClientInfo { name: "VTCode".into(), version: Some(env!("CARGO_PKG_VERSION").into()) }),
            workspace_folders: Some(vec![WorkspaceFolder { uri: root_uri.clone(), name: root.file_name().unwrap_or_default().to_string_lossy().into_owned() }]),
            root_uri: Some(root_uri),
            capabilities: ClientCapabilities {
                general: Some(GeneralClientCapabilities {
                    position_encodings: Some(vec![PositionEncodingKind::UTF16]),
                    ..Default::default()
                }),
                text_document: Some(TextDocumentClientCapabilities {
                    completion: Some(CompletionClientCapabilities {
                        completion_item: Some(CompletionItemCapability {
                            snippet_support: Some(true),
                            insert_replace_support: Some(true),
                            resolve_support: Some(CompletionItemCapabilityResolveSupport {
                                properties: vec!["documentation".into(), "detail".into(), "additionalTextEdits".into()],
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    hover: Some(HoverClientCapabilities {
                        content_format: Some(vec![MarkupKind::PlainText]),
                        ..Default::default()
                    }),
                    publish_diagnostics: Some(PublishDiagnosticsClientCapabilities {
                        related_information: Some(true),
                        version_support: Some(true),
                        data_support: Some(true),
                        ..Default::default()
                    }),
                    code_action: Some(CodeActionClientCapabilities {
                        // The workbench applies WorkspaceEdit code actions.
                        // Servers such as rust-analyzer omit those actions
                        // unless the client advertises literal support.
                        code_action_literal_support: Some(CodeActionLiteralSupport {
                            code_action_kind: CodeActionKindLiteralSupport {
                                value_set: ["", "quickfix", "refactor", "refactor.extract",
                                    "refactor.inline", "refactor.rewrite", "source",
                                    "source.organizeImports", "source.fixAll"]
                                    .into_iter().map(str::to_owned).collect(),
                            },
                        }),
                        ..Default::default()
                    }),
                    inlay_hint: Some(InlayHintClientCapabilities {
                        ..Default::default()
                    }),
                    semantic_tokens: Some(SemanticTokensClientCapabilities {
                        requests: SemanticTokensClientCapabilitiesRequests {
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                        },
                        token_types: vec![
                            SemanticTokenType::NAMESPACE,
                            SemanticTokenType::TYPE,
                            SemanticTokenType::CLASS,
                            SemanticTokenType::ENUM,
                            SemanticTokenType::INTERFACE,
                            SemanticTokenType::STRUCT,
                            SemanticTokenType::TYPE_PARAMETER,
                            SemanticTokenType::PARAMETER,
                            SemanticTokenType::VARIABLE,
                            SemanticTokenType::PROPERTY,
                            SemanticTokenType::ENUM_MEMBER,
                            SemanticTokenType::EVENT,
                            SemanticTokenType::FUNCTION,
                            SemanticTokenType::METHOD,
                            SemanticTokenType::MACRO,
                            SemanticTokenType::KEYWORD,
                            SemanticTokenType::MODIFIER,
                            SemanticTokenType::COMMENT,
                            SemanticTokenType::STRING,
                            SemanticTokenType::NUMBER,
                            SemanticTokenType::REGEXP,
                            SemanticTokenType::OPERATOR,
                            SemanticTokenType::DECORATOR,
                        ],
                        token_modifiers: vec![
                            SemanticTokenModifier::DECLARATION,
                            SemanticTokenModifier::DEFINITION,
                            SemanticTokenModifier::READONLY,
                            SemanticTokenModifier::STATIC,
                            SemanticTokenModifier::DEPRECATED,
                            SemanticTokenModifier::ABSTRACT,
                            SemanticTokenModifier::ASYNC,
                            SemanticTokenModifier::MODIFICATION,
                            SemanticTokenModifier::DOCUMENTATION,
                            SemanticTokenModifier::DEFAULT_LIBRARY,
                        ],
                        formats: vec![TokenFormat::RELATIVE],
                        ..Default::default()
                    }),
                    document_highlight: Some(DocumentHighlightClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    document_symbol: Some(DocumentSymbolClientCapabilities {
                        dynamic_registration: Some(false),
                        ..Default::default()
                    }),
                    folding_range: Some(FoldingRangeClientCapabilities {
                        dynamic_registration: Some(false),
                        ..Default::default()
                    }),
                    call_hierarchy: Some(CallHierarchyClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    selection_range: Some(SelectionRangeClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    code_lens: Some(CodeLensClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    type_hierarchy: Some(TypeHierarchyClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    document_link: Some(DocumentLinkClientCapabilities {
                        dynamic_registration: Some(false),
                        tooltip_support: Some(true),
                    }),
                    ..Default::default()
                }),
                workspace: Some(lsp_types::WorkspaceClientCapabilities {
                    apply_edit: Some(true),
                    workspace_folders: Some(true),
                    workspace_edit: Some(WorkspaceEditClientCapabilities {
                        document_changes: Some(true),
                        failure_handling: Some(FailureHandlingKind::Abort),
                        ..Default::default()
                    }),
                    did_change_watched_files: Some(
                        lsp_types::DidChangeWatchedFilesClientCapabilities {
                            dynamic_registration: Some(true),
                            relative_pattern_support: Some(false),
                        },
                    ),
                    configuration: Some(true),
                    file_operations: Some(lsp_types::WorkspaceFileOperationsClientCapabilities {
                        will_rename: Some(true),
                        did_rename: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            initialization_options: init_options,
            ..Default::default()
        };
        self.send_request(
            "initialize",
            Some(serde_json::to_value(&params).map_err(|e| e.to_string())?),
        )
    }

    /// Send the initialized notification (after initialize response received)
    pub(crate) fn send_initialized(&mut self) -> Result<(), String> {
        self.send_notification(
            "initialized",
            Some(serde_json::to_value(InitializedParams {}).map_err(|e| e.to_string())?),
        )?;
        self.initialized = true;
        if !self.settings.is_null() {
            self.send_notification("workspace/didChangeConfiguration", Some(serde_json::json!({"settings": self.settings})))?;
        }
        for (_, message) in std::mem::take(&mut self.pending_opens) {
            self.outbound_tx.send(message).map_err(|e| format!("LSP writer stopped: {e}"))?;
        }
        Ok(())
    }

    /// Shut down the server gracefully
    pub(crate) fn shutdown(&mut self) -> Result<(), String> {
        if matches!(self.child, crate::remote::lsp::LspProcess::Remote(_)) {
            let _ = self.send_notification("exit", None);
            let _ = self.child.kill();
            return Ok(());
        }
        if self.child.try_wait().ok().flatten().is_some() { return Ok(()); }
        if self.initialized {
            if let Ok(id) = self.send_request("shutdown", None) {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(1);
                while self.pending_requests.lock().is_ok_and(|pending| pending.contains_key(&id))
                    && std::time::Instant::now() < until
                    && self.child.try_wait().ok().flatten().is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            }
        }
        let _ = self.send_notification("exit", None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ = self.child.kill(); // Error OK: process may have already exited
                        let _ = self.child.wait(); // Error OK: process cleanup is best-effort
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        Ok(())
    }

    // ── Document Synchronization ──────────────────────────────────

    /// Notify server that a file has been opened
    #[cfg(test)]
    pub(crate) fn did_open(
        &mut self,
        path: &Path,
        language_id: &str,
        text: &str,
    ) -> Result<(), String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        self.did_open_owned(path, language_id.to_string(), uri, Rope::from_str(text))
    }

    fn did_open_owned(
        &mut self,
        path: &Path,
        language_id: String,
        uri: Url,
        text: Rope,
    ) -> Result<(), String> {
        let message = LspOutbound::DidOpen {
                uri: uri.to_string(),
                language_id,
                version: 0,
                text,
            };
        if self.initialized {
            self.outbound_tx.send(message).map_err(|error| format!("LSP writer stopped: {error}"))?;
        } else {
            self.pending_opens.insert(path.to_path_buf(), message);
        }
        self.open_files.insert(path.to_path_buf());
        Ok(())
    }

    /// Notify server that a file has been changed using its negotiated sync mode.
    #[cfg(test)]
    fn did_change(&mut self, path: &Path, version: i32, text: &str) -> Result<(), String> {
        self.did_change_owned(path, version, Rope::from_str(text))
    }

    fn did_change_owned(&mut self, path: &Path, version: i32, text: Rope) -> Result<(), String> {
        if let Some(LspOutbound::DidOpen { version: pending_version, text: pending_text, .. }) = self.pending_opens.get_mut(path) {
            *pending_version = version;
            *pending_text = text;
            return Ok(());
        }
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        self.outbound_tx
            .send(LspOutbound::DidChange {
                uri: uri.to_string(),
                version,
                text,
            })
            .map_err(|error| format!("LSP writer stopped: {error}"))
    }

    /// Notify server that a file has been closed
    pub(crate) fn did_close(&mut self, path: &Path) -> Result<(), String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        self.open_files.remove(path);
        if self.pending_opens.remove(path).is_some() { return Ok(()); }
        self.outbound_tx.send(LspOutbound::DidClose { uri })
            .map_err(|error| format!("LSP writer stopped: {error}"))
    }

    /// Notify server that a file was saved
    fn did_save(&mut self, path: &Path) -> Result<(), String> {
        if !self.initialized { return Ok(()); }
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier { uri },
            text: None,
        };
        self.send_notification(
            "textDocument/didSave",
            Some(serde_json::to_value(&params).map_err(|e| e.to_string())?),
        )
    }

    /// Notify server about watched file changes (workspace/didChangeWatchedFiles)
    fn did_change_watched_files(&mut self, changes: &[lsp_types::FileEvent]) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        let params = lsp_types::DidChangeWatchedFilesParams {
            changes: changes.to_vec(),
        };
        self.send_notification(
            "workspace/didChangeWatchedFiles",
            Some(serde_json::to_value(&params).map_err(|e| e.to_string())?),
        )
    }

    /// Respond to a workspace/configuration request from the server
    pub(crate) fn respond_configuration(
        &mut self,
        request_id: i64,
        settings: Vec<Value>,
    ) -> Result<(), String> {
        self.send_response(request_id, Value::Array(settings))
    }

    /// Send workspace/didRenameFiles notification after renaming
    pub(crate) fn did_rename_files(
        &mut self,
        old_path: &Path,
        new_path: &Path,
    ) -> Result<(), String> {
        let old_uri = Url::from_file_path(old_path)
            .map_err(|_| "Invalid old path")?
            .to_string();
        let new_uri = Url::from_file_path(new_path)
            .map_err(|_| "Invalid new path")?
            .to_string();
        let params = lsp_types::RenameFilesParams {
            files: vec![lsp_types::FileRename { old_uri, new_uri }],
        };
        self.send_notification(
            "workspace/didRenameFiles",
            Some(serde_json::to_value(&params).map_err(|e| e.to_string())?),
        )
    }

    /// Check if this client handles files with the given extension
    pub(crate) fn handles_file(&self, path: &Path) -> bool {
        if let Some((_, language)) = crate::file_associations::resolve(&self.file_associations, path) {
            return crate::file_associations::server_supports(&self.language, language);
        }
        matches_file_patterns(&self.file_patterns, path)
    }

    /// Check if a file is already opened on this client
    pub(crate) fn is_file_open(&self, path: &Path) -> bool {
        self.open_files.contains(path)
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.shutdown(); // Error OK: best-effort cleanup on Drop
    }
}

/// Simplified code action for UI rendering and application
#[derive(Debug, Clone)]
pub(crate) struct LspCodeAction {
    pub title: String,
    pub edit: Option<WorkspaceEdit>,
    pub kind: Option<String>,
    pub command: Option<lsp_types::Command>,
}

/// Manages multiple LSP clients, one per language.
pub(crate) struct LspManager {
    pub(crate) remote_workspace: Option<crate::remote::workspace::Workspace>,
    pub definition_dirty: bool,
    pub references_dirty: bool,
    pub format_request_target: Option<crate::background::LspDocumentTarget>,
    pub diagnostic_context: HashMap<PathBuf, diagnostics::DiagnosticReport>,
    pub rename_request_versions: HashMap<PathBuf, u64>,
    pub code_action_request_versions: HashMap<PathBuf, u64>,
    pub workspace_edit_requests: Vec<(u32, Value, WorkspaceEdit)>,
    /// Active LSP clients keyed by language id
    pub clients: Vec<LspClient>,
    /// LSP server configs from extensions
    pub configs: Vec<LspServerConfig>,
    pub(crate) file_associations: Arc<crate::file_associations::Associations>,
    pub(crate) startup_errors: HashMap<String, String>,
    /// Workspace root
    pub workspace_root: PathBuf,
    /// Wake the terminal loop exactly when a server publishes an event.
    pub(crate) wake: crate::background::UiWake,
    pub(crate) next_poll_client: usize,
    /// Accumulated diagnostics by file path
    pub diagnostics: HashMap<PathBuf, Vec<LspDiagnostic>>,
    /// Monotonic version counter for changes to displayed diagnostics.
    pub diagnostics_version: u64,
    diagnostic_rows: diagnostics::DiagnosticRows,
    /// Files edited locally but not yet synchronized with their language server.
    pub diagnostic_dirty_documents: HashSet<PathBuf>,
    pub(crate) diagnostic_publication: diagnostics::DiagnosticPublication,
    /// Pending completion items (latest response)
    pub completion_items: Vec<LspCompletionItem>,
    /// Invalidate presentation only when a list or resolved documentation changes.
    pub(crate) completion_revision: u64,
    pub(crate) completion_response_ready: bool,
    /// Pending hover content
    pub hover_content: Option<String>,
    /// ID of the active completion request (to discard stale results)
    pub active_completion_id: Option<i64>,
    /// ID of the active hover request
    pub active_hover_id: Option<i64>,
    /// Pending go-to-definition result
    pub definition_locations: Vec<Location>,
    /// ID of the active definition request
    pub active_definition_id: Option<i64>,
    /// Definition probes decorate Ctrl-hover without triggering navigation.
    pub(crate) active_definition_link_id: Option<i64>,
    pub(crate) definition_link_available: bool,
    /// ID of the active type definition request
    pub active_type_definition_id: Option<i64>,
    /// ID of the active implementation request
    pub active_implementation_id: Option<i64>,
    /// ID of the active declaration request
    pub active_declaration_id: Option<i64>,
    /// Pending signature help
    pub signature_help: Option<LspSignatureHelp>,
    pub(crate) signature_revision: u64,
    /// ID of the active signature help request
    pub active_signature_id: Option<i64>,
    /// Pending code actions
    pub code_actions: Vec<LspCodeAction>,
    /// ID of the active code action request
    pub active_code_action_id: Option<i64>,
    /// Pending format response edits
    pub format_edits: Vec<lsp_types::TextEdit>,
    /// ID of the active format request
    pub active_format_id: Option<i64>,
    /// True when format_edits were just updated
    pub format_dirty: bool,
    pub(crate) format_error: Option<String>,
    /// Pending rename response
    pub rename_edit: Option<lsp_types::WorkspaceEdit>,
    /// ID of the active rename request
    pub active_rename_id: Option<i64>,
    /// ID of the active prepare rename request
    pub active_prepare_rename_id: Option<i64>,
    /// Result of prepare rename (range, placeholder)
    pub prepare_rename_result: Option<(Option<lsp_types::Range>, Option<String>)>,
    /// Pending will_rename workspace edit
    pub will_rename_edit: Option<lsp_types::WorkspaceEdit>,
    /// ID of the active willRenameFiles request
    pub active_will_rename_id: Option<i64>,
    /// Pending references result
    pub reference_locations: Vec<Location>,
    /// ID of the active references request
    pub active_references_id: Option<i64>,
    /// Pending inlay hints for the current file
    pub inlay_hints: Vec<InlayHint>,
    /// ID of the active inlay hint request
    pub active_inlay_hint_id: Option<i64>,
    pub(crate) inlay_hint_request: Option<DocumentFeedbackRequest>,
    pub(crate) inlay_hint_response_target: Option<DocumentFeedbackRequest>,
    /// True when an inlay-hint response arrived, including an empty response.
    pub inlay_hints_dirty: bool,
    /// Raw semantic tokens awaiting worker-side relative-position decoding.
    pub semantic_tokens_response: Option<SemanticTokens>,
    /// ID of the active semantic tokens request
    pub active_semantic_tokens_id: Option<i64>,
    /// True when a semantic-token response arrived, including an empty response.
    pub semantic_tokens_dirty: bool,
    /// Semantic token legend from server (token type names)
    pub semantic_token_legend: Option<SemanticTokensLegend>,
    /// Pending document highlights for cursor position
    pub document_highlights: Vec<DocumentHighlight>,
    /// ID of the active document highlight request
    pub active_highlight_id: Option<i64>,
    /// Includes empty/unsupported outcomes, so idle frames do not retry them.
    pub(crate) highlight_request: Option<(DocumentFeedbackRequest, (u32, u32))>,
    /// True when document_highlights were just updated (needs processing)
    pub document_highlights_dirty: bool,
    /// Pending document symbol response (flattened to SymbolInformation)
    pub document_symbols: Vec<SymbolInformation>,
    /// ID of the active document symbol request
    pub active_document_symbol_id: Option<i64>,
    pub(crate) document_symbol_request: Option<DocumentFeedbackRequest>,
    pub(crate) document_symbol_response_target: Option<DocumentFeedbackRequest>,
    /// True when document_symbols were just updated
    pub document_symbols_dirty: bool,
    /// Pending folding range response from LSP
    pub folding_ranges: Vec<FoldingRange>,
    /// ID of the active folding range request
    pub active_folding_range_id: Option<i64>,
    /// True when folding_ranges were just updated
    pub folding_ranges_dirty: bool,
    /// Call hierarchy items from prepare request
    pub call_hierarchy_items: Vec<CallHierarchyItem>,
    pub active_call_hierarchy_id: Option<i64>,
    pub call_hierarchy_dirty: bool,
    /// Incoming calls result
    pub call_hierarchy_incoming: Vec<CallHierarchyIncomingCall>,
    pub active_incoming_calls_id: Option<i64>,
    pub call_hierarchy_incoming_dirty: bool,
    /// Outgoing calls result
    pub call_hierarchy_outgoing: Vec<CallHierarchyOutgoingCall>,
    pub active_outgoing_calls_id: Option<i64>,
    pub call_hierarchy_outgoing_dirty: bool,
    /// Selection range results from LSP (flattened from innermost to outermost)
    pub selection_ranges: Vec<SelectionRange>,
    pub active_selection_range_id: Option<i64>,
    pub(crate) active_selection_range_client: Option<usize>,
    pub selection_ranges_dirty: bool,
    /// Code lens results from LSP
    pub code_lenses: Vec<CodeLens>,
    pub active_code_lens_id: Option<i64>,
    pub code_lenses_dirty: bool,
    /// Resolved code lens results
    pub code_lens_resolve_results: Vec<CodeLens>,
    pub code_lens_resolve_dirty: bool,
    /// Type hierarchy items from prepare request
    pub type_hierarchy_items: Vec<lsp_types::TypeHierarchyItem>,
    pub active_type_hierarchy_id: Option<i64>,
    pub type_hierarchy_dirty: bool,
    /// Supertypes result
    pub type_hierarchy_supertypes: Vec<lsp_types::TypeHierarchyItem>,
    pub active_supertypes_id: Option<i64>,
    pub type_hierarchy_supertypes_dirty: bool,
    /// Subtypes result
    pub type_hierarchy_subtypes: Vec<lsp_types::TypeHierarchyItem>,
    pub active_subtypes_id: Option<i64>,
    pub type_hierarchy_subtypes_dirty: bool,
    /// Document links
    pub document_links: Vec<DocumentLink>,
    pub active_document_links_id: Option<i64>,
    pub document_links_dirty: bool,
    /// On-type formatting
    pub on_type_format_edits: Vec<lsp_types::TextEdit>,
    pub active_on_type_format_id: Option<i64>,
    pub on_type_format_dirty: bool,
    /// Trigger characters for on-type formatting (from server capabilities)
    pub on_type_trigger_chars: Vec<String>,
    /// Completion details belong to a row in the current response, not its label.
    pub(crate) completion_resolve_index: Option<usize>,
    pub(crate) completion_resolved_items: HashSet<usize>,
    pub active_resolve_id: Option<i64>,
    pub completion_resolve_dirty: bool,
    /// Workspace symbol results
    pub workspace_symbols: Vec<SymbolInformation>,
    pub active_workspace_symbol_id: Option<i64>,
    pub workspace_symbols_dirty: bool,
    /// Document versions per file (for didChange)
    pub doc_versions: HashMap<PathBuf, i32>,
    /// Last content_version we notified LSP about, per file path
    pub last_notified_version: HashMap<PathBuf, u64>,
    /// Failed active requests whose UI pending state must be retired.
    pub(crate) request_errors: Vec<String>,
}

/// Active signature help state for rendering
#[derive(Debug, Clone)]
pub(crate) struct LspSignatureHelp {
    pub signatures: Vec<LspSignatureInfo>,
    pub active_signature: usize,
    pub active_parameter: usize,
}

/// Simplified diagnostic for rendering
#[derive(Debug, Clone)]
pub(crate) struct LspDiagnostic {
    pub line: usize,
    pub col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub message: String,
    pub severity: DiagnosticSeverity,
}

/// Simplified completion item for rendering
#[derive(Debug, Clone)]
pub(crate) struct LspCompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub insert_text: String,
    pub kind: Option<CompletionItemKind>,
    /// LSP text edit range (line, col) for start and end of replacement
    pub text_edit_range: Option<((u32, u32), (u32, u32))>,
    /// Additional edits (e.g., auto-imports) as (path, Vec<TextEdit>)
    pub additional_text_edits: Vec<lsp_types::TextEdit>,
    /// Whether insert_text uses snippet syntax (InsertTextFormat::Snippet)
    pub is_snippet: bool,
    /// Original LSP CompletionItem for resolve requests
    pub original_item: Option<CompletionItem>,
    /// Resolved documentation from completionItem/resolve
    pub documentation: Option<String>,
}

/// Simplified signature information for rendering
#[derive(Debug, Clone)]
pub(crate) struct LspSignatureInfo {
    pub label: String,
    pub parameters: Vec<(usize, usize)>,
    pub documentation: Option<String>,
}

impl LspManager {
    #[cfg(test)]
    pub(crate) fn new(workspace_root: PathBuf) -> Self {
        Self::new_with_wake(workspace_root, crate::background::UiWake::default())
    }

    pub(crate) fn new_with_wake(
        workspace_root: PathBuf,
        wake: crate::background::UiWake,
    ) -> Self {
        Self {
            clients: Vec::new(),
            remote_workspace: None,
            configs: Vec::new(),
            file_associations: Arc::default(),
            startup_errors: HashMap::new(),
            workspace_root,
            wake,
            next_poll_client: 0,
            diagnostics: HashMap::new(),
            diagnostics_version: 0,
            diagnostic_rows: Default::default(),
            diagnostic_dirty_documents: HashSet::new(),
            diagnostic_publication: Default::default(),
            completion_items: Vec::new(),
            completion_revision: 0,
            completion_response_ready: false,
            hover_content: None,
            active_completion_id: None,
            active_hover_id: None,
            definition_locations: Vec::new(),
            active_definition_id: None,
            active_definition_link_id: None,
            definition_link_available: false,
            active_type_definition_id: None,
            active_implementation_id: None,
            active_declaration_id: None,
            signature_help: None,
            signature_revision: 0,
            active_signature_id: None,
            code_actions: Vec::new(),
            active_code_action_id: None,
            format_edits: Vec::new(),
            active_format_id: None,
            format_dirty: false,
            format_error: None,
            rename_edit: None,
            active_rename_id: None,
            active_prepare_rename_id: None,
            prepare_rename_result: None,
            will_rename_edit: None,
            active_will_rename_id: None,
            reference_locations: Vec::new(),
            active_references_id: None,
            inlay_hints: Vec::new(),
            active_inlay_hint_id: None,
            inlay_hint_request: None,
            inlay_hint_response_target: None,
            inlay_hints_dirty: false,
            semantic_tokens_response: None,
            active_semantic_tokens_id: None,
            semantic_tokens_dirty: false,
            semantic_token_legend: None,
            workspace_edit_requests: Vec::new(),
            definition_dirty: false,
            references_dirty: false,
            format_request_target: None,
            diagnostic_context: HashMap::new(),
            rename_request_versions: HashMap::new(),
            code_action_request_versions: HashMap::new(),
            document_highlights: Vec::new(),
            active_highlight_id: None,
            highlight_request: None,
            document_highlights_dirty: false,
            document_symbols: Vec::new(),
            active_document_symbol_id: None,
            document_symbol_request: None,
            document_symbol_response_target: None,
            document_symbols_dirty: false,
            folding_ranges: Vec::new(),
            active_folding_range_id: None,
            folding_ranges_dirty: false,
            call_hierarchy_items: Vec::new(),
            active_call_hierarchy_id: None,
            call_hierarchy_dirty: false,
            call_hierarchy_incoming: Vec::new(),
            active_incoming_calls_id: None,
            call_hierarchy_incoming_dirty: false,
            call_hierarchy_outgoing: Vec::new(),
            active_outgoing_calls_id: None,
            call_hierarchy_outgoing_dirty: false,
            selection_ranges: Vec::new(),
            active_selection_range_id: None,
            active_selection_range_client: None,
            selection_ranges_dirty: false,
            code_lenses: Vec::new(),
            active_code_lens_id: None,
            code_lenses_dirty: false,
            code_lens_resolve_results: Vec::new(),
            code_lens_resolve_dirty: false,
            type_hierarchy_items: Vec::new(),
            active_type_hierarchy_id: None,
            type_hierarchy_dirty: false,
            type_hierarchy_supertypes: Vec::new(),
            active_supertypes_id: None,
            type_hierarchy_supertypes_dirty: false,
            type_hierarchy_subtypes: Vec::new(),
            active_subtypes_id: None,
            type_hierarchy_subtypes_dirty: false,
            document_links: Vec::new(),
            active_document_links_id: None,
            document_links_dirty: false,
            on_type_format_edits: Vec::new(),
            active_on_type_format_id: None,
            on_type_format_dirty: false,
            on_type_trigger_chars: Vec::new(),
            completion_resolve_index: None,
            completion_resolved_items: HashSet::new(),
            active_resolve_id: None,
            completion_resolve_dirty: false,
            workspace_symbols: Vec::new(),
            active_workspace_symbol_id: None,
            workspace_symbols_dirty: false,
            doc_versions: HashMap::new(),
            last_notified_version: HashMap::new(),
            request_errors: Vec::new(),
        }
    }

    /// Register an LSP server config from an extension
    pub(crate) fn register_config(&mut self, config: LspServerConfig) {
        // Don't duplicate
        if self.configs.iter().any(|c| c.language == config.language) {
            return;
        }
        self.configs.push(config);
    }

    /// Whether a configured or already-running server handles this path. This
    /// check does not start a process, so it is safe to use before queuing a
    /// shared document snapshot.
    pub(crate) fn has_config_for_file(&self, path: &Path) -> bool {
        if self.clients.iter().any(|client| client.handles_file(path)) {
            return true;
        }
        self.configs.iter().find(|config| self.config_handles_file(config, path))
            .is_some_and(|config| !self.startup_errors.contains_key(&config.language))
    }

    fn config_handles_file(&self, config: &LspServerConfig, path: &Path) -> bool {
        if let Some((_, language)) = crate::file_associations::resolve(&self.file_associations, path) {
            return crate::file_associations::server_supports(&config.language, language);
        }
        config.handles_file(path)
    }

    /// Find or start a client for the given file path
    pub(crate) fn ensure_client_for_file(&mut self, path: &Path) -> Option<usize> {
        // Local preferences/assets can be edited in a remote workbench too.
        // Never send their native paths or contents to a remote language server.
        if self.remote_workspace.is_some() && !crate::remote::workspace::is_view_key(path) {
            return None;
        }
        // Check if an existing client handles this file
        if let Some(idx) = self.clients.iter().position(|c| c.handles_file(path)) {
            return Some(idx);
        }

        // Find a config that matches
        let config_idx = self.configs.iter().position(|config| self.config_handles_file(config, path));

        if let Some(ci) = config_idx {
            let config = self.configs[ci].clone();
            if self.startup_errors.contains_key(&config.language) {
                return None;
            }
            let started = match &self.remote_workspace {
                Some(remote) => LspClient::start_remote(&config, remote, self.wake.clone()),
                None => LspClient::start_with_wake(&config, &self.workspace_root, self.wake.clone()),
            };
            match started {
                Ok(mut client) => {
                    client.file_associations = Arc::clone(&self.file_associations);
                    self.clients.push(client);
                    Some(self.clients.len() - 1)
                }
                Err(error) => {
                    self.startup_errors.insert(config.language, error);
                    None
                }
            }
        } else {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn sync_document_owned(&mut self, path: &Path, content_version: u64, text: String) {
        self.sync_document(path, content_version, Rope::from_str(&text));
    }

    /// Queue a shared snapshot. Diffing, serialization and I/O run on the writer
    /// in protocol order, before requests made against this document version.
    pub(crate) fn sync_document(&mut self, path: &Path, content_version: u64, text: Rope) {
        let Some(idx) = self.ensure_client_for_file(path) else {
            return;
        };

        if !self.clients[idx].is_file_open(path) {
            let language = self.clients[idx].document_language(path);
            let Ok(uri) = Url::from_file_path(path) else {
                return;
            };
            if self.clients[idx]
                .did_open_owned(path, language, uri, text)
                .is_ok()
            {
                self.doc_versions.insert(path.to_path_buf(), 0);
                self.last_notified_version
                    .insert(path.to_path_buf(), content_version);
                self.diagnostic_dirty_documents.remove(path);
            }
            return;
        }

        let last = self.last_notified_version.get(path).copied().unwrap_or(0);
        if content_version <= last {
            return;
        }
        let version = self.doc_versions.entry(path.to_path_buf()).or_insert(0);
        *version += 1;
        if self.clients[idx]
            .did_change_owned(path, *version, text)
            .is_ok()
        {
            self.last_notified_version
                .insert(path.to_path_buf(), content_version);
            self.diagnostic_dirty_documents.remove(path);
        }
    }

    /// Notify an LSP server of a text change
    #[cfg(test)]
    pub(crate) fn did_change(&mut self, path: &Path, text: &str) {
        if let Some(idx) = self
            .clients
            .iter()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            let version = self.doc_versions.entry(path.to_path_buf()).or_insert(0);
            *version += 1;
            let v = *version;
            let _ = self.clients[idx].did_change(path, v, text); // Error OK: LSP notification is best-effort, server may be down
            self.diagnostic_dirty_documents.remove(path);
        }
    }

    /// Notify an LSP server of a file close
    pub(crate) fn did_close(&mut self, path: &Path) {
        self.cancel_document_feedback_for(path);
        if let Some(idx) = self
            .clients
            .iter()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            let _ = self.clients[idx].did_close(path); // Error OK: LSP notification is best-effort, server may be down
        }
        // Clean up per-file state for the closed file
        self.diagnostics.remove(path);
        self.diagnostic_context.remove(path);
        self.diagnostics_version += 1;
        self.diagnostic_dirty_documents.remove(path);
        self.doc_versions.remove(path);
        self.last_notified_version.remove(path);
        self.diagnostic_publication.forget(path);
    }

    /// Invalidate outstanding feedback requests tied to previous contents.
    /// The last published diagnostics remain visible until a fresh publish
    /// replaces them; removing them here made every edit flash squiggles,
    /// overview markers and problem counts off and back on.
    pub(crate) fn invalidate_document_feedback(&mut self, path: &Path) {
        self.diagnostic_dirty_documents.insert(path.to_path_buf());
        self.diagnostic_publication.edited(path, std::time::Instant::now());

        self.cancel_document_feedback_for(path);
        let id = self.active_semantic_tokens_id.take();
        self.cancel_request_id(id);
        self.semantic_tokens_response = None;
        self.semantic_tokens_dirty = false;
        let id = self.active_on_type_format_id.take();
        self.cancel_request_id(id);
        self.on_type_format_dirty = false;
    }

    /// Send size-limited notifications for a coalesced filesystem burst.
    pub(crate) fn notify_did_change_watched_files<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) {
        if !self.clients.iter().any(|client| client.initialized) { return; }
        const MAX_BYTES: usize = 256 * 1024;
        let mut changes = Vec::new();
        let mut bytes = 0;
        for path in paths {
            let Ok(uri) = Url::from_file_path(path) else { continue; };
            let size = uri.as_str().len() + 64;
            if !changes.is_empty() && bytes + size > MAX_BYTES {
                self.send_watched_file_changes(&changes);
                changes.clear();
                bytes = 0;
            }
            bytes += size;
            changes.push(lsp_types::FileEvent { uri, typ: lsp_types::FileChangeType::CHANGED });
        }
        self.send_watched_file_changes(&changes);
    }

    fn send_watched_file_changes(&mut self, changes: &[lsp_types::FileEvent]) {
        if changes.is_empty() { return; }
        for client in &mut self.clients {
            if client.initialized {
                let _ = client.did_change_watched_files(changes);
                // Error OK: LSP notification is best-effort, server may be down
            }
        }
    }

    /// Send workspace/didRenameFiles notification after rename
    pub(crate) fn did_rename_files(&mut self, old_path: &Path, new_path: &Path) {
        for client in &mut self.clients {
            if client.initialized {
                let _ = client.did_rename_files(old_path, new_path); // Error OK: LSP notification is best-effort, server may be down
            }
        }
    }

    /// Cancel a globally unique request ID on its owning server.
    pub(crate) fn cancel_request_id(&mut self, id: Option<i64>) {
        if let Some(id) = id {
            for client in &mut self.clients { client.cancel_request(id); }
        }
    }

    pub(crate) fn cancel_completion_request(&mut self) {
        self.completion_response_ready = false;
        let id = self.active_completion_id.take();
        self.cancel_request_id(id);
        self.cancel_completion_resolve();
        self.completion_resolved_items.clear();
        self.completion_resolve_dirty = false;
    }

    pub(crate) fn cancel_signature_request(&mut self) {
        let id = self.active_signature_id.take();
        self.cancel_request_id(id);
    }

    /// Request completions
    pub(crate) fn request_completion(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        prefix: &str,
    ) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_completion(path, line, col, prefix) {
                self.cancel_completion_request();
                self.active_completion_id = Some(id);
                self.completion_items.clear();
                return true;
            }
        }
        false
    }

    /// Request workspace symbols matching a query
    pub(crate) fn request_workspace_symbol(&mut self, query: &str) -> bool {
        // Try any initialized client
        for client in &mut self.clients {
            if client.initialized {
                if let Ok(id) = client.request_workspace_symbol(query) {
                    self.active_workspace_symbol_id = Some(id);
                    return true;
                }
            }
        }
        false
    }

    pub(crate) fn completion_match_rank(label_lower: &str, prefix_lower: &str) -> u8 {
        if prefix_lower.is_empty() || label_lower.starts_with(prefix_lower) {
            0
        } else if label_lower.contains(prefix_lower) {
            1
        } else {
            2
        }
    }

    pub(crate) fn completion_kind_rank(kind: Option<CompletionItemKind>) -> u8 {
        match kind {
            Some(CompletionItemKind::FUNCTION) | Some(CompletionItemKind::METHOD) => 0,
            Some(CompletionItemKind::VARIABLE)
            | Some(CompletionItemKind::FIELD)
            | Some(CompletionItemKind::PROPERTY)
            | Some(CompletionItemKind::CONSTANT) => 1,
            Some(CompletionItemKind::CLASS)
            | Some(CompletionItemKind::STRUCT)
            | Some(CompletionItemKind::INTERFACE)
            | Some(CompletionItemKind::ENUM)
            | Some(CompletionItemKind::ENUM_MEMBER) => 2,
            Some(CompletionItemKind::KEYWORD) => 3,
            Some(CompletionItemKind::MODULE) => 4,
            Some(CompletionItemKind::SNIPPET) => 5,
            _ => 6,
        }
    }

    /// Request hover
    pub(crate) fn request_hover(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.initialized && !c.server_dead && c.handles_file(path)
                && c.is_file_open(path) && c.supports_request("textDocument/hover"))
        {
            if let Ok(id) = self.clients[idx].request_hover(path, line, col) {
                self.active_hover_id = Some(id);
                self.hover_content = None;
                return true;
            }
        }
        false
    }

    /// Request go-to-definition
    pub(crate) fn request_definition(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_definition(path, line, col) {
                self.active_definition_id = Some(id);
                self.definition_locations.clear();
                return true;
            }
        }
        false
    }

    /// Request go-to-type-definition
    pub(crate) fn request_type_definition(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_type_definition(path, line, col) {
                self.active_type_definition_id = Some(id);
                self.definition_locations.clear();
                return true;
            }
        }
        false
    }

    /// Request go-to-implementation
    pub(crate) fn request_implementation(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_implementation(path, line, col) {
                self.active_implementation_id = Some(id);
                self.definition_locations.clear();
                return true;
            }
        }
        false
    }

    /// Request go-to-declaration
    pub(crate) fn request_declaration(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_declaration(path, line, col) {
                self.active_declaration_id = Some(id);
                self.definition_locations.clear();
                return true;
            }
        }
        false
    }

    /// Request signature help
    pub(crate) fn request_signature_help(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_signature_help(path, line, col) {
                self.active_signature_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request code actions for a range
    pub(crate) fn request_code_action(&mut self, p: &CodeActionParams) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(p.path) && c.is_file_open(p.path))
        {
            if let Ok(id) = self.clients[idx].request_code_action(p) {
                self.code_action_request_versions = self.last_notified_version.clone();
                self.active_code_action_id = Some(id);
                self.code_actions.clear();
                return true;
            }
        }
        false
    }

    /// Request document formatting
    pub(crate) fn request_format_document(
        &mut self,
        path: &Path,
        tab_size: u32,
        insert_spaces: bool,
    ) -> bool {
        self.format_error = None;
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            match self.clients[idx].request_format_document(path, tab_size, insert_spaces) {
                Ok(id) => {
                    self.format_request_target = self.last_notified_version.get(path).map(|&content_version| crate::background::LspDocumentTarget { path:path.to_path_buf(), content_version });
                    self.active_format_id = Some(id);
                    self.format_edits.clear();
                    self.format_dirty = false;
                    return true;
                }
                Err(error) => self.format_error = Some(error),
            }
        }
        false
    }

    /// Request range formatting (format selection)
    pub(crate) fn request_format_range(&mut self, p: &FormatRangeParams) -> bool {
        self.format_error = None;
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(p.path) && c.is_file_open(p.path))
        {
            match self.clients[idx].request_format_range(p) {
                Ok(id) => {
                    self.format_request_target = self.last_notified_version.get(p.path).map(|&content_version| crate::background::LspDocumentTarget { path:p.path.to_path_buf(), content_version });
                    self.active_format_id = Some(id);
                    self.format_edits.clear();
                    self.format_dirty = false;
                    return true;
                }
                Err(error) => self.format_error = Some(error),
            }
        }
        false
    }

    /// Request rename symbol
    pub(crate) fn request_prepare_rename(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_prepare_rename(path, line, col) {
                self.active_prepare_rename_id = Some(id);
                self.prepare_rename_result = None;
                return true;
            }
        }
        false
    }

    /// Request rename symbol
    pub(crate) fn request_rename(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        new_name: &str,
    ) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_rename(path, line, col, new_name) {
                self.rename_request_versions = self.last_notified_version.clone();
                self.active_rename_id = Some(id);
                self.rename_edit = None;
                return true;
            }
        }
        false
    }

    /// Request find all references
    pub(crate) fn request_references(&mut self, path: &Path, line: u32, col: u32) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_references(path, line, col) {
                self.active_references_id = Some(id);
                self.reference_locations.clear();
                return true;
            }
        }
        false
    }

    /// Request inlay hints for a visible range
    pub(crate) fn request_inlay_hints(
        &mut self,
        path: &Path,
        start_line: u32,
        end_line: u32,
    ) -> bool {
        let Some(version) = self.last_notified_version.get(path).copied() else {
            return false;
        };
        let range = Some((start_line, end_line));
        if self
            .inlay_hint_request
            .as_ref()
            .is_some_and(|request| request.matches(path, version, range))
        {
            return true;
        }
        self.cancel_inlay_hint_request();
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.initialized && !c.server_dead && c.handles_file(path) && c.is_file_open(path))
        {
            self.inlay_hint_request = Some(DocumentFeedbackRequest {
                target: crate::background::LspDocumentTarget {
                    path: path.to_path_buf(),
                    content_version: version,
                },
                range,
                client_idx: idx,
            });
            if !self.clients[idx].supports_request("textDocument/inlayHint") {
                return true;
            }
            if let Ok(id) = self.clients[idx].request_inlay_hints(path, start_line, end_line) {
                self.active_inlay_hint_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request semantic tokens for the full document
    pub(crate) fn request_semantic_tokens_full(&mut self, path: &Path) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_semantic_tokens_full(path) {
                self.active_semantic_tokens_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request document highlights for symbol under cursor
    pub(crate) fn request_document_highlight(&mut self, path: &Path, line: u32, col: u32) -> bool {
        let Some(version) = self.last_notified_version.get(path).copied() else { return false; };
        if self.highlight_request.as_ref().is_some_and(|(request, position)| {
            request.matches(path, version, None) && *position == (line, col)
        }) { return true; }
        self.cancel_document_highlight();
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.initialized && !c.server_dead && c.handles_file(path) && c.is_file_open(path))
        {
            self.highlight_request = Some((DocumentFeedbackRequest {
                target: crate::background::LspDocumentTarget { path: path.to_path_buf(), content_version: version },
                range: None,
                client_idx: idx,
            }, (line, col)));
            if !self.clients[idx].supports_request("textDocument/documentHighlight") { return true; }
            if let Ok(id) = self.clients[idx].request_document_highlight(path, line, col) {
                self.active_highlight_id = Some(id);
                return true;
            }
        }
        false
    }

    pub(crate) fn cancel_document_highlight(&mut self) {
        self.highlight_request = None;
        let id = self.active_highlight_id.take();
        self.cancel_request_id(id);
        self.document_highlights_dirty = false;
        self.document_highlights.clear();
    }

    /// Called before draining replies, including when typing delays the next
    /// request. A buffered result belongs to its original caret and document.
    pub(crate) fn retain_document_highlight(&mut self, current: Option<(&Path, u64, u32, u32)>) {
        if self.highlight_request.as_ref().is_some_and(|(request, position)| {
            !current.is_some_and(|(path, version, line, col)| {
                request.matches(path, version, None) && *position == (line, col)
            })
        }) { self.cancel_document_highlight(); }
    }

    /// Request document symbols for outline
    pub(crate) fn request_document_symbol(&mut self, path: &Path) -> bool {
        let Some(version) = self.last_notified_version.get(path).copied() else {
            return false;
        };
        if self
            .document_symbol_request
            .as_ref()
            .is_some_and(|request| request.matches(path, version, None))
        {
            return true;
        }
        self.cancel_document_symbol_request();
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.initialized && !c.server_dead && c.handles_file(path) && c.is_file_open(path))
        {
            self.document_symbol_request = Some(DocumentFeedbackRequest {
                target: crate::background::LspDocumentTarget {
                    path: path.to_path_buf(),
                    content_version: version,
                },
                range: None,
                client_idx: idx,
            });
            if !self.clients[idx].supports_request("textDocument/documentSymbol") {
                return true;
            }
            if let Ok(id) = self.clients[idx].request_document_symbol(path) {
                self.active_document_symbol_id = Some(id);
                return true;
            }
        }
        false
    }

    pub(crate) fn cancel_inlay_hint_request(&mut self) {
        if let (Some(request), Some(id)) = (
            self.inlay_hint_request.take(),
            self.active_inlay_hint_id.take(),
        ) {
            if let Some(client) = self.clients.get_mut(request.client_idx) {
                client.cancel_request(id);
            }
        }
        self.inlay_hint_response_target = None;
        self.inlay_hints_dirty = false;
        self.inlay_hints.clear();
    }

    pub(crate) fn cancel_document_symbol_request(&mut self) {
        let request = self.document_symbol_request.take();
        let obsolete = [
            self.active_document_symbol_id.take(),
            self.active_folding_range_id.take(),
            self.active_code_lens_id.take(),
            self.active_document_links_id.take(),
        ];
        if let Some(request) = request {
            if let Some(client) = self.clients.get_mut(request.client_idx) {
                for id in obsolete.into_iter().flatten() {
                    client.cancel_request(id);
                }
            }
        }
        self.document_symbol_response_target = None;
        self.document_symbols_dirty = false;
        self.document_symbols.clear();
        // These requests share the outline's document snapshot and are only
        // reissued after synchronization. Suppress late previous-file replies.
        self.folding_ranges_dirty = false;
        self.code_lenses_dirty = false;
        self.document_links_dirty = false;
    }

    fn cancel_document_feedback_for(&mut self, path: &Path) {
        if self.highlight_request.as_ref().is_some_and(|(request, _)| request.target.path == path) {
            self.cancel_document_highlight();
        }
        if self
            .inlay_hint_request
            .as_ref()
            .is_some_and(|request| request.target.path == path)
        {
            self.cancel_inlay_hint_request();
        }
        if self
            .document_symbol_request
            .as_ref()
            .is_some_and(|request| request.target.path == path)
        {
            self.cancel_document_symbol_request();
        }
    }

    /// Cancel before polling replies, even while the latest editor snapshot is
    /// still waiting for didChange. The editor itself retains the desired state.
    pub(crate) fn retain_document_feedback(
        &mut self,
        document: Option<(&Path, u64)>,
        inlay_range: Option<(u32, u32)>,
    ) {
        let keep_inlay = self.inlay_hint_request.as_ref().is_none_or(|request| {
            document.is_some_and(|(path, version)| {
                inlay_range.is_some() && request.matches(path, version, inlay_range)
            })
        });
        if !keep_inlay {
            self.cancel_inlay_hint_request();
        }
        let keep_symbols = self.document_symbol_request.as_ref().is_none_or(|request| {
            document.is_some_and(|(path, version)| request.matches(path, version, None))
        });
        if !keep_symbols {
            self.cancel_document_symbol_request();
        }
    }

    /// JSON-RPC errors complete a request, rather than terminating its server.
    /// Passive keys remain recorded, including unsupported/empty outcomes.
    pub(crate) fn finish_failed_request(
        &mut self,
        client_idx: usize,
        id: i64,
        method: &str,
    ) -> bool {
        match method {
            "initialize" => {
                self.clients[client_idx].server_dead = true;
                return false;
            }
            "textDocument/inlayHint" => {
                if self.active_inlay_hint_id == Some(id)
                    && self
                        .inlay_hint_request
                        .as_ref()
                        .is_some_and(|request| request.client_idx == client_idx)
                {
                    self.active_inlay_hint_id = None;
                    self.inlay_hint_response_target = self.inlay_hint_request.clone();
                    self.inlay_hints.clear();
                    self.inlay_hints_dirty = true;
                }
                return false;
            }
            "textDocument/documentSymbol" => {
                if self.active_document_symbol_id == Some(id)
                    && self
                        .document_symbol_request
                        .as_ref()
                        .is_some_and(|request| request.client_idx == client_idx)
                {
                    self.active_document_symbol_id = None;
                    self.document_symbol_response_target = self.document_symbol_request.clone();
                    self.document_symbols.clear();
                    self.document_symbols_dirty = true;
                }
                return false;
            }
            _ => {}
        }
        let active = match method {
            "textDocument/completion" => &mut self.active_completion_id,
            "completionItem/resolve" => &mut self.active_resolve_id,
            "workspace/symbol" => &mut self.active_workspace_symbol_id,
            "textDocument/hover" => &mut self.active_hover_id,
            "textDocument/definition" => &mut self.active_definition_id,
            "textDocument/typeDefinition" => &mut self.active_type_definition_id,
            "textDocument/implementation" => &mut self.active_implementation_id,
            "textDocument/declaration" => &mut self.active_declaration_id,
            "textDocument/references" => &mut self.active_references_id,
            "textDocument/signatureHelp" => &mut self.active_signature_id,
            "textDocument/codeAction" => &mut self.active_code_action_id,
            "textDocument/formatting" | "textDocument/rangeFormatting" => {
                &mut self.active_format_id
            }
            "textDocument/onTypeFormatting" => &mut self.active_on_type_format_id,
            "textDocument/prepareRename" => &mut self.active_prepare_rename_id,
            "textDocument/rename" => &mut self.active_rename_id,
            "workspace/willRenameFiles" => &mut self.active_will_rename_id,
            "textDocument/semanticTokens/full" => &mut self.active_semantic_tokens_id,
            "textDocument/documentHighlight" => &mut self.active_highlight_id,
            "textDocument/foldingRange" => &mut self.active_folding_range_id,
            "textDocument/prepareCallHierarchy" => &mut self.active_call_hierarchy_id,
            "callHierarchy/incomingCalls" => &mut self.active_incoming_calls_id,
            "callHierarchy/outgoingCalls" => &mut self.active_outgoing_calls_id,
            "textDocument/selectionRange" => {
                if self.active_selection_range_client != Some(client_idx) { return false; }
                &mut self.active_selection_range_id
            }
            "textDocument/codeLens" => &mut self.active_code_lens_id,
            "textDocument/documentLink" => &mut self.active_document_links_id,
            "textDocument/prepareTypeHierarchy" => &mut self.active_type_hierarchy_id,
            "typeHierarchy/supertypes" => &mut self.active_supertypes_id,
            "typeHierarchy/subtypes" => &mut self.active_subtypes_id,
            _ => return false,
        };
        if *active == Some(id) {
            *active = None;
            if method == "textDocument/selectionRange" {
                self.active_selection_range_client = None;
            }
            if matches!(
                method,
                "textDocument/formatting" | "textDocument/rangeFormatting"
            ) {
                // Preserve the normal format-on-save completion/fallback path.
                self.format_edits.clear();
                self.format_dirty = true;
            }
            return true;
        }
        false
    }

    /// Request folding ranges for a file
    pub(crate) fn request_folding_range(&mut self, path: &Path) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_folding_range(path) {
                self.active_folding_range_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request prepare call hierarchy at cursor position
    pub(crate) fn request_prepare_call_hierarchy(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
    ) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_prepare_call_hierarchy(path, line, col) {
                self.active_call_hierarchy_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request incoming calls for a call hierarchy item
    pub(crate) fn request_incoming_calls(&mut self, item: &CallHierarchyItem) -> bool {
        // Route the follow-up to the server that handles the hierarchy item.
        let Ok(path) = item.uri.to_file_path() else { return false; };
        for client in self.clients.iter_mut().filter(|client| client.handles_file(&path)) {
            if let Ok(id) = client.request_incoming_calls(item) {
                self.active_incoming_calls_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request outgoing calls for a call hierarchy item
    pub(crate) fn request_outgoing_calls(&mut self, item: &CallHierarchyItem) -> bool {
        let Ok(path) = item.uri.to_file_path() else { return false; };
        for client in self.clients.iter_mut().filter(|client| client.handles_file(&path)) {
            if let Ok(id) = client.request_outgoing_calls(item) {
                self.active_outgoing_calls_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request selection ranges at given positions
    pub(crate) fn request_selection_range(
        &mut self,
        path: &Path,
        positions: Vec<Position>,
    ) -> bool {
        self.cancel_selection_range_request();
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_selection_range(path, positions) {
                self.active_selection_range_id = Some(id);
                self.active_selection_range_client = Some(idx);
                return true;
            }
        }
        false
    }

    pub(crate) fn cancel_selection_range_request(&mut self) {
        if let (Some(client), Some(id)) = (
            self.active_selection_range_client.take(), self.active_selection_range_id.take(),
        ) {
            if let Some(client) = self.clients.get_mut(client) { client.cancel_request(id); }
        }
        self.selection_ranges.clear();
        self.selection_ranges_dirty = false;
    }

    /// Request code lenses for a file
    pub(crate) fn request_code_lens(&mut self, path: &Path) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_code_lens(path) {
                self.active_code_lens_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Resolve a code lens to get its command details
    pub(crate) fn request_code_lens_resolve(&mut self, path: &Path, lens: &CodeLens) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(_id) = self.clients[idx].request_code_lens_resolve(lens) {
                return true;
            }
        }
        false
    }

    /// Request prepare type hierarchy at cursor position
    pub(crate) fn request_prepare_type_hierarchy(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
    ) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_prepare_type_hierarchy(path, line, col) {
                self.active_type_hierarchy_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request supertypes for a type hierarchy item
    pub(crate) fn request_supertypes(&mut self, item: &lsp_types::TypeHierarchyItem) -> bool {
        let Ok(path) = item.uri.to_file_path() else { return false; };
        for client in self.clients.iter_mut().filter(|client| client.handles_file(&path)) {
            if let Ok(id) = client.request_supertypes(item) {
                self.active_supertypes_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request subtypes for a type hierarchy item
    pub(crate) fn request_subtypes(&mut self, item: &lsp_types::TypeHierarchyItem) -> bool {
        let Ok(path) = item.uri.to_file_path() else { return false; };
        for client in self.clients.iter_mut().filter(|client| client.handles_file(&path)) {
            if let Ok(id) = client.request_subtypes(item) {
                self.active_subtypes_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request document links for a file
    pub(crate) fn request_document_links(&mut self, path: &Path) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            if let Ok(id) = self.clients[idx].request_document_links(path) {
                self.active_document_links_id = Some(id);
                return true;
            }
        }
        false
    }

    /// Request on-type formatting
    pub(crate) fn request_on_type_formatting(&mut self, p: &OnTypeFormattingParams) -> bool {
        if let Some(idx) = self
            .clients
            .iter_mut()
            .position(|c| c.handles_file(p.path) && c.is_file_open(p.path))
        {
            if let Ok(id) = self.clients[idx].request_on_type_formatting(p) {
                self.active_on_type_format_id = Some(id);
                return true;
            }
        }
        false
    }

    // poll() is in lsp_events.rs

    /// Get diagnostics for a specific file
    pub(crate) fn get_diagnostics(&self, path: &Path) -> Option<&Vec<LspDiagnostic>> {
        self.diagnostics.get(path)
    }

    /// Check if any LSP server is active for diagnostics
    #[cfg(test)]
    pub(crate) fn has_diagnostics(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    /// Notify LSP of content changes if the version has changed since last notification
    #[cfg(test)]
    pub(crate) fn notify_if_changed(&mut self, path: &Path, content_version: u64, text: &str) {
        let last = self.last_notified_version.get(path).copied().unwrap_or(0);
        if content_version > last {
            self.last_notified_version
                .insert(path.to_path_buf(), content_version);
            self.did_change(path, text);
        }
    }

    /// Notify LSP that a file was saved
    pub(crate) fn notify_did_save(&mut self, path: &Path) {
        if let Some(idx) = self
            .clients
            .iter()
            .position(|c| c.handles_file(path) && c.is_file_open(path))
        {
            let _ = self.clients[idx].did_save(path); // Error OK: LSP notification is best-effort, server may be down
        }
    }

    /// Shut down all clients
    pub(crate) fn shutdown_all(&mut self) {
        self.cancel_inlay_hint_request();
        self.cancel_document_symbol_request();
        for client in &mut self.clients {
            let _ = client.shutdown(); // Error OK: best-effort cleanup during server removal
        }
        self.clients.clear();
    }

    /// Get list of active LSP server languages (for status bar display)
    #[cfg(test)]
    pub(crate) fn active_server_names(&self) -> Vec<&str> {
        self.clients.iter().map(|c| c.language.as_str()).collect()
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}

// Re-export for tests (markup_content_to_string lives in lsp_parsing)
#[cfg(test)]
pub(crate) use crate::lsp_parsing::markup_content_to_string;

/// Map diagnostic severity to a display character
pub(crate) fn severity_icon(severity: DiagnosticSeverity) -> &'static str {
    if severity == DiagnosticSeverity::ERROR {
        "✗"
    } else if severity == DiagnosticSeverity::WARNING {
        "⚠"
    } else if severity == DiagnosticSeverity::INFORMATION {
        "ℹ"
    } else {
        "·"
    }
}

/// Map diagnostic severity to a color
#[cfg(test)]
pub(crate) fn severity_color(severity: DiagnosticSeverity) -> ratatui::style::Color {
    use ratatui::style::Color;
    if severity == DiagnosticSeverity::ERROR {
        Color::Rgb(255, 80, 80)
    } else if severity == DiagnosticSeverity::WARNING {
        Color::Rgb(255, 200, 50)
    } else if severity == DiagnosticSeverity::INFORMATION {
        Color::Rgb(100, 180, 255)
    } else {
        Color::Rgb(150, 150, 150)
    }
}

#[cfg(test)]
#[path = "lsp_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lsp_real_tests.rs"]
mod real_tests;

#[path = "lsp_capabilities.rs"]
mod capabilities;

#[path = "lsp_server_requests.rs"]
mod server_requests;

#[cfg(test)]
#[path = "lsp_protocol_tests.rs"]
mod protocol_tests;
