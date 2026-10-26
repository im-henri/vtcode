//! LSP message parsing and reading logic.
//!
//! Extracted from `lsp.rs` – contains the JSON-RPC message reader,
//! the response/notification parser, and small helpers they use.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::{mpsc, Arc, Mutex};

use lsp_types::*;
use serde_json::Value;

use crate::lsp::{LspClient, LspCodeAction, LspEvent, LspSignatureInfo, PendingRequest};

/// Convert an LSP MarkedString to plain text
pub(crate) fn markup_content_to_string(mc: MarkedString) -> String {
    match mc {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(ls) => ls.value,
    }
}

impl LspClient {
    pub(crate) fn reader_loop_mapped(stdout: Box<dyn std::io::Read + Send>, tx: mpsc::Sender<LspEvent>, pending_requests: Arc<Mutex<HashMap<i64, PendingRequest>>>, wake: crate::background::UiWake,
        remote: Option<crate::remote::workspace::Workspace>) {
        let mut reader = BufReader::new(stdout);
        loop {
            match Self::read_message(&mut reader) {
                Ok(Some(msg)) => {
                    let event = serde_json::from_str(&msg).ok().and_then(|mut message| {
                        if let Some(remote) = &remote { remote.rewrite_lsp(&mut message, false); }
                        Self::parse_value(message, &pending_requests)
                    });
                    if let Some(event) = event {
                        if tx.send(event).is_err() {
                            break; // main thread dropped the receiver
                        }
                        wake.notify();
                    }
                }
                Ok(None) => {
                    if tx.send(LspEvent::ServerError(())).is_ok() {
                        wake.notify();
                    }
                    break;
                }
                Err(_e) => {
                    if tx.send(LspEvent::ServerError(())).is_ok() {
                        wake.notify();
                    }
                    break;
                }
            }
        }
    }

    /// Read one LSP message (Content-Length header + body)
    pub(crate) fn read_message(reader: &mut impl BufRead) -> Result<Option<String>, String> {
        let mut content_length: Option<usize> = None;
        const MAX_HEADERS: usize = 16 * 1024;
        const MAX_BODY: usize = 64 * 1024 * 1024;
        let mut header_bytes = 0;

        // Read headers
        loop {
            let mut header = String::new();
            match std::io::Read::take(&mut *reader,(MAX_HEADERS - header_bytes + 1) as u64).read_line(&mut header) {
                Ok(0) if header_bytes == 0 => return Ok(None),
                Ok(0) => return Err("Truncated LSP headers".into()),
                Err(e) => return Err(format!("Read error: {}", e)),
                _ => {}
            }
            header_bytes += header.len();
            if header_bytes > MAX_HEADERS { return Err("LSP headers exceed the size limit".into()); }
            let header = header.trim();
            if header.is_empty() {
                break; // End of headers
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("Content-Length")
            {
                let length = value.trim().parse::<usize>().map_err(|_| "Invalid LSP Content-Length")?;
                if length > MAX_BODY { return Err("LSP message exceeds the size limit".into()); }
                if content_length.replace(length).is_some() { return Err("Duplicate LSP Content-Length".into()); }
            }
        }

        let len = content_length.ok_or_else(|| "Missing Content-Length header".to_string())?;

        // Read body
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).map_err(|e| format!("Body read error: {}", e))?;

        String::from_utf8(body).map(Some).map_err(|e| format!("UTF-8 error: {}", e))
    }

    pub(crate) fn parameter_label_offsets(offsets: &[u32]) -> (usize, usize) {
        match (offsets.first(), offsets.get(1)) {
            (Some(&start), Some(&end)) => (start as usize, end as usize),
            (Some(&start), None) => (start as usize, start as usize),
            _ => (0, 0),
        }
    }

    /// Parse a JSON-RPC message into an LspEvent
    #[cfg(test)]
    pub(crate) fn parse_message(json: &str, pending_requests: &Arc<Mutex<HashMap<i64, PendingRequest>>>) -> Option<LspEvent> {
        Self::parse_value(serde_json::from_str(json).ok()?, pending_requests)
    }
    fn parse_value(mut msg: Value, pending_requests: &Arc<Mutex<HashMap<i64, PendingRequest>>>) -> Option<LspEvent> {

        // Check if it's a notification (no id)
        if let Some("textDocument/publishDiagnostics") = msg.get("method").and_then(|m| m.as_str()) {
            if let Some(params) = msg.get_mut("params").map(Value::take) {
                let pd: PublishDiagnosticsParams = serde_json::from_value(params).ok()?;
                return Some(LspEvent::Diagnostics {
                    uri: pd.uri,
                    version: pd.version,
                    diagnostics: pd.diagnostics,
                });
            }
        }

        // Handle server-to-client requests (has both method and id)
        if let (Some(method), Some(id)) = (msg.get("method").and_then(|m| m.as_str()), msg.get("id").filter(|id| id.is_i64() || id.is_string())) {
            let method = method.to_owned();
            let request_id = id.clone();
            if method == "workspace/configuration" && let Some(id) = request_id.as_i64() {
                if let Some(params) = msg.get_mut("params").map(Value::take) {
                    if let Ok(config_params) = serde_json::from_value::<lsp_types::ConfigurationParams>(params) {
                        return Some(LspEvent::WorkspaceConfigurationRequest {
                            request_id: id,
                            items: config_params.items,
                        });
                    }
                }
                // Even if parsing fails, return empty config request so we can respond
                return Some(LspEvent::WorkspaceConfigurationRequest {
                    request_id: id,
                    items: Vec::new(),
                });
            }
            return Some(LspEvent::ServerRequest {
                request_id,
                method,
                params: msg.get_mut("params").map(Value::take).unwrap_or(Value::Null),
            });
        }

        // Check if it's a response (has id)
        if let Some(id) = msg.get("id").and_then(|id| id.as_i64()) {
            // Look up the method for this request ID
            let request = pending_requests.lock().ok().and_then(|mut map| map.remove(&id));
            let completion_prefix = request.as_ref().and_then(|request| request.completion_prefix.as_deref());
            let method = request.as_ref().map(|request| request.method.as_str());

            if let Some(result) = msg.get_mut("result").map(Value::take) {
                match method {
                    Some("textDocument/completion") => {
                        if result.is_null() {
                            return Some(LspEvent::CompletionResponse { request_id: id, items: Vec::new() });
                        }
                        return Some(match serde_json::from_value::<CompletionResponse>(result) {
                            Ok(response) => {
                                let items = match response {
                                    CompletionResponse::Array(items) => items,
                                    CompletionResponse::List(list) => list.items,
                                };
                                LspEvent::CompletionResponse {
                                    request_id: id,
                                    items: crate::lsp::completion::prepare_completions(items, completion_prefix),
                                }
                            }
                            Err(error) => LspEvent::RequestError {
                                request_id: id, method: "textDocument/completion".into(),
                                message: format!("The language server returned an invalid completion response: {error}"),
                            },
                        });
                    }
                    Some("textDocument/signatureHelp") => {
                        if result.is_null() || result.get("signatures").and_then(Value::as_array).is_some_and(Vec::is_empty) {
                            return Some(LspEvent::SignatureHelpResponse { request_id: id, signatures: Vec::new(), active_signature: 0, active_parameter: 0 });
                        }
                        if let Ok(sig_help) = serde_json::from_value::<SignatureHelp>(result) {
                            if !sig_help.signatures.is_empty() {
                                let active_sig = sig_help.active_signature.unwrap_or(0) as usize;
                                let active_param = sig_help.signatures.get(active_sig).and_then(|sig| sig.active_parameter).or(sig_help.active_parameter).unwrap_or(0) as usize;
                                let signatures = sig_help.signatures.into_iter().map(|sig| {
                                    let params: Vec<(usize, usize)> = sig.parameters.unwrap_or_default().into_iter().map(|p| {
                                        match p.label {
                                            ParameterLabel::LabelOffsets(offsets) => {
                                                let (start, end) = Self::parameter_label_offsets(&offsets);
                                                let label = ropey::Rope::from_str(&sig.label);
                                                let byte = |units: usize| label.char_to_byte(label.utf16_cu_to_char(units.min(label.len_utf16_cu())));
                                                (byte(start), byte(end))
                                            },
                                            ParameterLabel::Simple(s) => {
                                                if let Some(start) = sig.label.find(&s) {
                                                    (start, start + s.len())
                                                } else {
                                                    (0, 0)
                                                }
                                            }
                                        }
                                    }).collect();
                                    let doc = sig.documentation.map(|d| match d {
                                        Documentation::String(s) => s,
                                        Documentation::MarkupContent(mc) => mc.value,
                                    });
                                    LspSignatureInfo {
                                        label: sig.label,
                                        parameters: params,
                                        documentation: doc,
                                    }
                                }).collect();
                                return Some(LspEvent::SignatureHelpResponse {
                                    request_id: id,
                                    signatures,
                                    active_signature: active_sig,
                                    active_parameter: active_param,
                                });
                            }
                        }
                    }
                    Some(method @ ("textDocument/formatting" | "textDocument/rangeFormatting")) => {
                        if result.is_null() {
                            return Some(LspEvent::FormatResponse {
                                request_id: id,
                                edits: Vec::new(),
                            });
                        }
                        return Some(match serde_json::from_value::<Vec<lsp_types::TextEdit>>(result) {
                            Ok(edits) => LspEvent::FormatResponse { request_id: id, edits },
                            Err(error) => LspEvent::RequestError {
                                request_id: id, method: method.into(),
                                message: format!("The language server returned an invalid formatting response: {error}"),
                            },
                        });
                    }
                    Some("textDocument/onTypeFormatting") => {
                        if result.is_null() {
                            return Some(LspEvent::OnTypeFormattingResponse {
                                request_id: id,
                                edits: Vec::new(),
                            });
                        }
                        if let Ok(edits) = serde_json::from_value::<Vec<lsp_types::TextEdit>>(result) {
                            return Some(LspEvent::OnTypeFormattingResponse {
                                request_id: id,
                                edits,
                            });
                        }
                    }
                    Some("textDocument/rename") => {
                        if let Ok(edit) = serde_json::from_value::<lsp_types::WorkspaceEdit>(result) {
                            return Some(LspEvent::RenameResponse {
                                request_id: id,
                                edit: Some(edit),
                            });
                        }
                    }
                    Some("workspace/willRenameFiles") => {
                        let edit = serde_json::from_value::<lsp_types::WorkspaceEdit>(result).ok();
                        return Some(LspEvent::WillRenameFilesResponse {
                            request_id: id,
                            edit,
                        });
                    }
                    Some("textDocument/codeAction") => {
                        if result.is_null() { return Some(LspEvent::CodeActionResponse { request_id:id, actions:Vec::new() }); }
                        if let Ok(actions) = serde_json::from_value::<Vec<CodeActionOrCommand>>(result) {
                            let lsp_actions: Vec<LspCodeAction> = actions.into_iter().map(|a| {
                                match a {
                                    CodeActionOrCommand::CodeAction(ca) => LspCodeAction {
                                        title: ca.title,
                                        edit: ca.edit,
                                        kind: ca.kind.map(|k| k.as_str().to_string()),
                                        command: ca.command,
                                    },
                                    CodeActionOrCommand::Command(cmd) => LspCodeAction {
                                        title: cmd.title.clone(),
                                        edit: None,
                                        kind: None,
                                        command: Some(cmd),
                                    },
                                }
                            }).collect();
                            return Some(LspEvent::CodeActionResponse {
                                request_id: id,
                                actions: lsp_actions,
                            });
                        }
                    }
                    Some("textDocument/hover") => {
                        if result.is_null() { return Some(LspEvent::HoverResponse { request_id:id, contents:None }); }
                        if let Ok(hover) = serde_json::from_value::<Hover>(result) {
                            let contents = match hover.contents {
                                HoverContents::Scalar(mc) => Some(markup_content_to_string(mc)),
                                HoverContents::Array(mcs) => {
                                    let joined = mcs.into_iter().enumerate().fold(String::new(), |mut acc, (i, mc)| {
                                        if i > 0 { acc.push('\n'); }
                                        acc.push_str(&markup_content_to_string(mc));
                                        acc
                                    });
                                    Some(joined)
                                }
                                HoverContents::Markup(mc) => Some(mc.value),
                            };
                            return Some(LspEvent::HoverResponse {
                                request_id: id,
                                contents,
                            });
                        }
                    }
                    Some("textDocument/definition" | "textDocument/typeDefinition" | "textDocument/implementation" | "textDocument/declaration" | "textDocument/references") => {
                        let locations = if result.is_null() { Vec::new() } else {
                            match serde_json::from_value::<GotoDefinitionResponse>(result).ok()? {
                                GotoDefinitionResponse::Scalar(location) => vec![location],
                                GotoDefinitionResponse::Array(locations) => locations,
                                GotoDefinitionResponse::Link(links) => links.into_iter().map(|link| Location { uri:link.target_uri, range:link.target_selection_range }).collect(),
                            }
                        };
                        return Some(LspEvent::DefinitionResponse { request_id:id, locations });
                    }
                    Some("textDocument/inlayHint") => {
                        if result.is_null() {
                            return Some(LspEvent::InlayHintResponse {
                                request_id: id,
                                hints: Vec::new(),
                            });
                        }
                        if let Ok(hints) = serde_json::from_value::<Vec<InlayHint>>(result) {
                            return Some(LspEvent::InlayHintResponse {
                                request_id: id,
                                hints,
                            });
                        }
                    }
                    Some("textDocument/semanticTokens/full") => {
                        if let Ok(result) = serde_json::from_value::<SemanticTokensResult>(result) {
                            let tokens = match result {
                                SemanticTokensResult::Tokens(t) => t,
                                SemanticTokensResult::Partial(p) => SemanticTokens { result_id: None, data: p.data },
                            };
                            return Some(LspEvent::SemanticTokensResponse {
                                request_id: id,
                                tokens,
                            });
                        }
                    }
                    Some("textDocument/documentHighlight") => {
                        if result.is_null() {
                            return Some(LspEvent::DocumentHighlightResponse {
                                request_id: id,
                                highlights: Vec::new(),
                            });
                        }
                        if let Ok(highlights) = serde_json::from_value::<Vec<DocumentHighlight>>(result) {
                            return Some(LspEvent::DocumentHighlightResponse {
                                request_id: id,
                                highlights,
                            });
                        }
                    }
                    Some("textDocument/documentSymbol") => {
                        if result.is_null() {
                            return Some(LspEvent::DocumentSymbolResponse {
                                request_id: id,
                                symbols: Vec::new(),
                            });
                        }
                        // Try DocumentSymbol[] first (hierarchical), flatten to SymbolInformation
                        if let Ok(doc_symbols) = serde_json::from_value::<DocumentSymbolResponse>(result) {
                            let mut flat_symbols = Vec::new();
                            match doc_symbols {
                                DocumentSymbolResponse::Flat(symbols) => {
                                    flat_symbols = symbols;
                                }
                                DocumentSymbolResponse::Nested(symbols) => {
                                    fn flatten_doc_symbols(symbols: &[DocumentSymbol], uri: &Url, out: &mut Vec<SymbolInformation>) {
                                        for sym in symbols {
                                            // SymbolInformation is deprecated in lsp_types but still the common
                                            // denominator returned by document/symbol and workspace/symbol responses.
                                            #[allow(deprecated)]
                                            out.push(SymbolInformation {
                                                name: sym.name.clone(),
                                                kind: sym.kind,
                                                tags: sym.tags.clone(),
                                                deprecated: sym.deprecated,
                                                location: Location {
                                                    uri: uri.clone(),
                                                    range: sym.selection_range,
                                                },
                                                container_name: None,
                                            });
                                            if let Some(ref children) = sym.children {
                                                flatten_doc_symbols(children, uri, out);
                                            }
                                        }
                                    }
                                    if let Ok(dummy_uri) = Url::parse("file:///") {
                                        flatten_doc_symbols(&symbols, &dummy_uri, &mut flat_symbols);
                                    }
                                }
                            }
                            return Some(LspEvent::DocumentSymbolResponse {
                                request_id: id,
                                symbols: flat_symbols,
                            });
                        }
                    }
                    Some("textDocument/foldingRange") => {
                        if result.is_null() {
                            return Some(LspEvent::FoldingRangeResponse {
                                request_id: id,
                                ranges: Vec::new(),
                            });
                        }
                        if let Ok(ranges) = serde_json::from_value::<Vec<FoldingRange>>(result) {
                            return Some(LspEvent::FoldingRangeResponse {
                                request_id: id,
                                ranges,
                            });
                        }
                    }
                    Some("textDocument/prepareCallHierarchy") => {
                        if result.is_null() {
                            return Some(LspEvent::CallHierarchyPrepareResponse {
                                request_id: id,
                                items: Vec::new(),
                            });
                        }
                        if let Ok(items) = serde_json::from_value::<Vec<CallHierarchyItem>>(result) {
                            return Some(LspEvent::CallHierarchyPrepareResponse {
                                request_id: id,
                                items,
                            });
                        }
                    }
                    Some("callHierarchy/incomingCalls") => {
                        if result.is_null() {
                            return Some(LspEvent::CallHierarchyIncomingResponse {
                                request_id: id,
                                calls: Vec::new(),
                            });
                        }
                        if let Ok(calls) = serde_json::from_value::<Vec<CallHierarchyIncomingCall>>(result) {
                            return Some(LspEvent::CallHierarchyIncomingResponse {
                                request_id: id,
                                calls,
                            });
                        }
                    }
                    Some("callHierarchy/outgoingCalls") => {
                        if result.is_null() {
                            return Some(LspEvent::CallHierarchyOutgoingResponse {
                                request_id: id,
                                calls: Vec::new(),
                            });
                        }
                        if let Ok(calls) = serde_json::from_value::<Vec<CallHierarchyOutgoingCall>>(result) {
                            return Some(LspEvent::CallHierarchyOutgoingResponse {
                                request_id: id,
                                calls,
                            });
                        }
                    }
                    Some("textDocument/selectionRange") => {
                        if result.is_null() {
                            return Some(LspEvent::SelectionRangeResponse {
                                request_id: id,
                                ranges: Vec::new(),
                            });
                        }
                        if let Ok(ranges) = serde_json::from_value::<Vec<SelectionRange>>(result) {
                            return Some(LspEvent::SelectionRangeResponse {
                                request_id: id,
                                ranges,
                            });
                        }
                    }
                    Some("textDocument/codeLens") => {
                        if result.is_null() {
                            return Some(LspEvent::CodeLensResponse {
                                request_id: id,
                                lenses: Vec::new(),
                            });
                        }
                        if let Ok(lenses) = serde_json::from_value::<Vec<CodeLens>>(result) {
                            return Some(LspEvent::CodeLensResponse {
                                request_id: id,
                                lenses,
                            });
                        }
                    }
                    Some("codeLens/resolve") => {
                        if let Ok(lens) = serde_json::from_value::<CodeLens>(result) {
                            return Some(LspEvent::CodeLensResolveResponse {
                                _request_id: id,
                                lens,
                            });
                        }
                    }
                    Some("textDocument/prepareTypeHierarchy") => {
                        if result.is_null() {
                            return Some(LspEvent::TypeHierarchyPrepareResponse {
                                request_id: id,
                                items: Vec::new(),
                            });
                        }
                        if let Ok(items) = serde_json::from_value::<Vec<lsp_types::TypeHierarchyItem>>(result) {
                            return Some(LspEvent::TypeHierarchyPrepareResponse {
                                request_id: id,
                                items,
                            });
                        }
                    }
                    Some("typeHierarchy/supertypes") => {
                        if result.is_null() {
                            return Some(LspEvent::TypeHierarchySupertypesResponse {
                                request_id: id,
                                items: Vec::new(),
                            });
                        }
                        if let Ok(items) = serde_json::from_value::<Vec<lsp_types::TypeHierarchyItem>>(result) {
                            return Some(LspEvent::TypeHierarchySupertypesResponse {
                                request_id: id,
                                items,
                            });
                        }
                    }
                    Some("typeHierarchy/subtypes") => {
                        if result.is_null() {
                            return Some(LspEvent::TypeHierarchySubtypesResponse {
                                request_id: id,
                                items: Vec::new(),
                            });
                        }
                        if let Ok(items) = serde_json::from_value::<Vec<lsp_types::TypeHierarchyItem>>(result) {
                            return Some(LspEvent::TypeHierarchySubtypesResponse {
                                request_id: id,
                                items,
                            });
                        }
                    }
                    Some("initialize") => {
                        if let Ok(init_result) = serde_json::from_value::<InitializeResult>(result) {
                            return Some(LspEvent::Initialized {
                                capabilities: Box::new(Some(init_result.capabilities)),
                            });
                        }
                    }
                    Some("textDocument/documentLink") => {
                        if result.is_null() {
                            return Some(LspEvent::DocumentLinkResponse {
                                request_id: id,
                                links: Vec::new(),
                            });
                        }
                        if let Ok(links) = serde_json::from_value::<Vec<DocumentLink>>(result) {
                            return Some(LspEvent::DocumentLinkResponse {
                                request_id: id,
                                links,
                            });
                        }
                    }
                    Some("completionItem/resolve") => {
                        return Some(match serde_json::from_value::<CompletionItem>(result) {
                            Ok(item) => LspEvent::CompletionResolveResponse {
                                request_id: id,
                                item: Box::new(item),
                            },
                            Err(error) => LspEvent::RequestError {
                                request_id: id,
                                method: "completionItem/resolve".into(),
                                message: format!("Invalid completion details: {error}"),
                            },
                        });
                    }
                    Some("workspace/symbol") => {
                        if result.is_null() {
                            return Some(LspEvent::WorkspaceSymbolResponse {
                                request_id: id,
                                symbols: Vec::new(),
                            });
                        }
                        if let Ok(symbols) = serde_json::from_value::<Vec<SymbolInformation>>(result.clone()) {
                            return Some(LspEvent::WorkspaceSymbolResponse {
                                request_id: id,
                                symbols,
                            });
                        }
                        // Try WorkspaceSymbolResponse format (newer LSP)
                        if let Ok(symbols) = serde_json::from_value::<Vec<lsp_types::WorkspaceSymbol>>(result) {
                            let converted: Vec<SymbolInformation> = symbols.into_iter().map(|ws| {
                                let location = match ws.location {
                                    lsp_types::OneOf::Left(loc) => loc,
                                    lsp_types::OneOf::Right(wsl) => Location {
                                        uri: wsl.uri,
                                        range: Range::default(),
                                    },
                                };
                                // See flatten_doc_symbols: deprecated but still the common type.
                                #[allow(deprecated)]
                                SymbolInformation {
                                    name: ws.name,
                                    kind: ws.kind,
                                    tags: ws.tags,
                                    deprecated: None,
                                    location,
                                    container_name: ws.container_name,
                                }
                            }).collect();
                            return Some(LspEvent::WorkspaceSymbolResponse {
                                request_id: id,
                                symbols: converted,
                            });
                        }
                    }
                    Some("textDocument/prepareRename") => {
                        if result.is_null() {
                            return Some(LspEvent::PrepareRenameResponse {
                                request_id: id,
                                range: None,
                                placeholder: None,
                            });
                        }
                        // Try PrepareRenameResponse variants:
                        // 1. { range, placeholder }
                        if let Some(obj) = result.as_object() {
                            if let (Some(range_val), Some(placeholder_val)) = (obj.get("range"), obj.get("placeholder")) {
                                if let (Ok(range), Some(placeholder)) = (
                                    serde_json::from_value::<lsp_types::Range>(range_val.clone()),
                                    placeholder_val.as_str(),
                                ) {
                                    return Some(LspEvent::PrepareRenameResponse {
                                        request_id: id,
                                        range: Some(range),
                                        placeholder: Some(placeholder.to_string()),
                                    });
                                }
                            }
                            // 2. { defaultBehavior: true } - use word at cursor
                            if obj.get("defaultBehavior").and_then(|v: &serde_json::Value| v.as_bool()) == Some(true) {
                                return Some(LspEvent::PrepareRenameResponse {
                                    request_id: id,
                                    range: None,
                                    placeholder: None,
                                });
                            }
                        }
                        // 3. Range only
                        if let Ok(range) = serde_json::from_value::<lsp_types::Range>(result) {
                            return Some(LspEvent::PrepareRenameResponse {
                                request_id: id,
                                range: Some(range),
                                placeholder: None,
                            });
                        }
                    }
                    _ => {
                        // Unknown method or missing - fall through
                    }
                }
            }

            // Check for error response
            if msg.get("error").is_some() {
                // Cancelled requests were removed from the pending map and
                // need no dispatch. A method error does not kill the server.
                return method.map(|method| LspEvent::RequestError {
                    request_id: id,
                    method: method.into(),
                    message: msg.get("error").and_then(|error| error.get("message"))
                        .and_then(Value::as_str).unwrap_or("The language server rejected the request").into(),
                });
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn make_pending(entries: &[(i64, &str)]) -> Arc<Mutex<HashMap<i64, PendingRequest>>> {
        let map: HashMap<i64, PendingRequest> = entries
            .iter()
            .map(|(id, method)| (*id, (*method).into()))
            .collect();
        Arc::new(Mutex::new(map))
    }

    fn empty_pending() -> Arc<Mutex<HashMap<i64, PendingRequest>>> {
        make_pending(&[])
    }

    // ── markup_content_to_string ──────────────────────────────────

    #[test]
    fn markup_content_to_string_plain_string() {
        let mc = MarkedString::String("hello world".to_string());
        assert_eq!(markup_content_to_string(mc), "hello world");
    }

    #[test]
    fn markup_content_to_string_language_string() {
        let mc = MarkedString::LanguageString(LanguageString {
            language: "rust".to_string(),
            value: "fn foo() {}".to_string(),
        });
        assert_eq!(markup_content_to_string(mc), "fn foo() {}");
    }

    // ── parameter_label_offsets ───────────────────────────────────

    #[test]
    fn parameter_label_offsets_basic() {
        assert_eq!(LspClient::parameter_label_offsets(&[3, 10]), (3, 10));
    }

    #[test]
    fn parameter_label_offsets_empty_slice() {
        assert_eq!(LspClient::parameter_label_offsets(&[]), (0, 0));
    }

    #[test]
    fn parameter_label_offsets_single_element() {
        assert_eq!(LspClient::parameter_label_offsets(&[7]), (7, 7));
    }

    // ── parse_message: completion ─────────────────────────────────

    #[test]
    fn parse_completion_items_array() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 1,
            "result": [
                {"label": "foo", "kind": 6},
                {"label": "bar", "kind": 6},
                {"label": "baz", "kind": 3}
            ]
        }"#;
        let pending = make_pending(&[(1, "textDocument/completion")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::CompletionResponse { request_id, items } => {
                assert_eq!(request_id, 1);
                assert_eq!(items.len(), 3);
                assert_eq!(items[0].label, "foo");
                assert_eq!(items[2].label, "baz");
            }
            other => panic!("Expected CompletionResponse, got {other:?}"),
        }
        // Pending entry should have been consumed
        assert!(pending.lock().unwrap().is_empty());
    }

    // ── parse_message: hover ──────────────────────────────────────

    #[test]
    fn parse_hover_scalar_marked_string() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "contents": "Some hover text"
            }
        }"#;
        let pending = make_pending(&[(2, "textDocument/hover")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::HoverResponse { request_id, contents } => {
                assert_eq!(request_id, 2);
                assert_eq!(contents.unwrap(), "Some hover text");
            }
            other => panic!("Expected HoverResponse, got {other:?}"),
        }
    }

    #[test]
    fn parse_hover_markup_content() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "contents": {"kind": "markdown", "value": "**bold** docs"}
            }
        }"#;
        let pending = make_pending(&[(3, "textDocument/hover")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::HoverResponse { request_id, contents } => {
                assert_eq!(request_id, 3);
                assert_eq!(contents.unwrap(), "**bold** docs");
            }
            other => panic!("Expected HoverResponse, got {other:?}"),
        }
    }

    // ── parse_message: definition ─────────────────────────────────

    #[test]
    fn parse_definition_single_location() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 4,
            "result": {
                "uri": "file:///src/main.rs",
                "range": {
                    "start": {"line": 5, "character": 4},
                    "end": {"line": 5, "character": 12}
                }
            }
        }"#;
        let pending = make_pending(&[(4, "textDocument/definition")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::DefinitionResponse { request_id, locations } => {
                assert_eq!(request_id, 4);
                assert_eq!(locations.len(), 1);
                assert_eq!(locations[0].range.start.line, 5);
                assert_eq!(locations[0].range.start.character, 4);
                assert_eq!(locations[0].range.end.character, 12);
            }
            other => panic!("Expected DefinitionResponse, got {other:?}"),
        }
    }

    // ── parse_message: diagnostics ────────────────────────────────

    #[test]
    fn parse_diagnostics_notification() {
        let json = r#"{
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///project/foo.rs",
                "diagnostics": [
                    {
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 5}
                        },
                        "severity": 2,
                        "message": "unused variable"
                    },
                    {
                        "range": {
                            "start": {"line": 10, "character": 3},
                            "end": {"line": 10, "character": 8}
                        },
                        "severity": 1,
                        "message": "type mismatch"
                    }
                ]
            }
        }"#;
        let event = LspClient::parse_message(json, &empty_pending()).unwrap();
        match event {
            LspEvent::Diagnostics { uri, diagnostics, .. } => {
                assert!(uri.path().ends_with("/foo.rs"));
                assert_eq!(diagnostics.len(), 2);
                assert_eq!(diagnostics[0].message, "unused variable");
                assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::WARNING));
                assert_eq!(diagnostics[1].message, "type mismatch");
                assert_eq!(diagnostics[1].severity, Some(DiagnosticSeverity::ERROR));
            }
            other => panic!("Expected Diagnostics, got {other:?}"),
        }
    }

    // ── parse_message: code action ────────────────────────────────

    #[test]
    fn parse_code_action_response() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 6,
            "result": [
                {
                    "title": "Add import",
                    "kind": "quickfix"
                },
                {
                    "title": "Extract function",
                    "kind": "refactor.extract"
                }
            ]
        }"#;
        let pending = make_pending(&[(6, "textDocument/codeAction")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::CodeActionResponse { request_id, actions } => {
                assert_eq!(request_id, 6);
                assert_eq!(actions.len(), 2);
                assert_eq!(actions[0].title, "Add import");
                assert_eq!(actions[0].kind.as_deref(), Some("quickfix"));
                assert_eq!(actions[1].title, "Extract function");
                assert_eq!(actions[1].kind.as_deref(), Some("refactor.extract"));
            }
            other => panic!("Expected CodeActionResponse, got {other:?}"),
        }
    }

    // ── parse_message: format ─────────────────────────────────────

    #[test]
    fn parse_format_response_with_edits() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 7,
            "result": [
                {
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 4}
                    },
                    "newText": "    "
                }
            ]
        }"#;
        let pending = make_pending(&[(7, "textDocument/formatting")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::FormatResponse { request_id, edits } => {
                assert_eq!(request_id, 7);
                assert_eq!(edits.len(), 1);
                assert_eq!(edits[0].new_text, "    ");
                assert_eq!(edits[0].range.start.line, 0);
            }
            other => panic!("Expected FormatResponse, got {other:?}"),
        }
    }

    // ── parse_message: unknown request ID ─────────────────────────

    #[test]
    fn parse_unknown_request_id_returns_none() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 999,
            "result": [{"label": "something"}]
        }"#;
        // No entry in pending_requests for id 999
        let result = LspClient::parse_message(json, &empty_pending());
        assert!(result.is_none());
    }

    // ── parse_message: malformed JSON ─────────────────────────────

    #[test]
    fn parse_malformed_json_returns_none() {
        let result = LspClient::parse_message("{invalid json!!!}", &empty_pending());
        assert!(result.is_none());
    }

    // ── parse_message: empty/null result ──────────────────────────

    #[test]
    fn parse_null_result_formatting_returns_empty_edits() {
        let json = r#"{"jsonrpc": "2.0", "id": 50, "result": null}"#;
        let pending = make_pending(&[(50, "textDocument/formatting")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::FormatResponse { request_id, edits } => {
                assert_eq!(request_id, 50);
                assert!(edits.is_empty());
            }
            other => panic!("Expected FormatResponse with empty edits, got {other:?}"),
        }
    }

    // ── parse_message: document symbol ────────────────────────────

    #[test]
    fn parse_document_symbol_flat_response() {
        #[allow(deprecated)]
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 60,
            "result": [
                {
                    "name": "MyStruct",
                    "kind": 23,
                    "location": {
                        "uri": "file:///src/lib.rs",
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 5, "character": 1}
                        }
                    }
                },
                {
                    "name": "my_func",
                    "kind": 12,
                    "location": {
                        "uri": "file:///src/lib.rs",
                        "range": {
                            "start": {"line": 7, "character": 0},
                            "end": {"line": 10, "character": 1}
                        }
                    }
                }
            ]
        }"#;
        let pending = make_pending(&[(60, "textDocument/documentSymbol")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::DocumentSymbolResponse { request_id, symbols } => {
                assert_eq!(request_id, 60);
                assert_eq!(symbols.len(), 2);
                assert_eq!(symbols[0].name, "MyStruct");
                assert_eq!(symbols[0].kind, SymbolKind::STRUCT);
                assert_eq!(symbols[1].name, "my_func");
                assert_eq!(symbols[1].kind, SymbolKind::FUNCTION);
            }
            other => panic!("Expected DocumentSymbolResponse, got {other:?}"),
        }
    }

    // ── parse_message: error response ─────────────────────────────

    #[test]
    fn parse_error_response_retires_only_the_request() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 70,
            "error": {
                "code": -32601,
                "message": "Method not found"
            }
        }"#;
        let pending = make_pending(&[(70, "textDocument/completion")]);
        let event = LspClient::parse_message(json, &pending).unwrap();
        match event {
            LspEvent::RequestError {
                request_id: 70,
                method,
                message,
            } if method == "textDocument/completion" && message == "Method not found" => {}
            other => panic!("Expected RequestError, got {other:?}"),
        }
    }

    // ── parse_message: pending map is consumed ────────────────────

    #[test]
    fn parse_message_removes_entry_from_pending_map() {
        let json = r#"{
            "jsonrpc": "2.0",
            "id": 80,
            "result": {"contents": "docs"}
        }"#;
        let pending = make_pending(&[(80, "textDocument/hover"), (81, "textDocument/completion")]);
        let _event = LspClient::parse_message(json, &pending);
        let map = pending.lock().unwrap();
        // ID 80 should be consumed, ID 81 should remain
        assert!(!map.contains_key(&80));
        assert!(map.contains_key(&81));
    }
}
