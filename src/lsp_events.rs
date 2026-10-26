//! LSP event types and poll dispatch.
//!
//! Contains the `LspEvent` enum (messages from LSP background readers)
//! and the `LspManager::poll()` method that dispatches them.

use lsp_types::*;
use serde_json::Value;

use crate::lsp::{
    LspCodeAction, LspCompletionItem, LspManager, LspSignatureHelp,
    LspSignatureInfo,
};

/// Messages from the LSP background reader to the main thread.
#[derive(Debug)]
pub(crate) enum LspEvent {
    /// Diagnostics for a file
    Diagnostics {
        uri: Url,
        version: Option<i32>,
        diagnostics: Vec<Diagnostic>,
    },
    /// Response to a completion request
    CompletionResponse {
        request_id: i64,
        items: Vec<LspCompletionItem>,
    },
    /// Response to a hover request
    HoverResponse {
        request_id: i64,
        contents: Option<String>,
    },
    /// Response to go-to-definition request
    DefinitionResponse {
        request_id: i64,
        locations: Vec<Location>,
    },
    /// Response to signature help request
    SignatureHelpResponse {
        request_id: i64,
        signatures: Vec<LspSignatureInfo>,
        active_signature: usize,
        active_parameter: usize,
    },
    /// Response to code action request
    CodeActionResponse {
        request_id: i64,
        actions: Vec<LspCodeAction>,
    },
    /// Response to format document request
    FormatResponse {
        request_id: i64,
        edits: Vec<lsp_types::TextEdit>,
    },
    /// Response to on-type formatting request
    OnTypeFormattingResponse {
        request_id: i64,
        edits: Vec<lsp_types::TextEdit>,
    },
    /// Response to rename request
    RenameResponse {
        request_id: i64,
        edit: Option<lsp_types::WorkspaceEdit>,
    },
    /// Server exited or error
    ServerError(()),
    /// A single JSON-RPC request failed; the server transport remains usable.
    RequestError { request_id: i64, method: String, message: String },
    /// Server has been initialized
    Initialized {
        capabilities: Box<Option<ServerCapabilities>>,
    },
    /// Response to inlay hints request
    InlayHintResponse {
        request_id: i64,
        hints: Vec<InlayHint>,
    },
    /// Response to semantic tokens request
    SemanticTokensResponse {
        request_id: i64,
        tokens: SemanticTokens,
    },
    /// Response to document highlight request
    DocumentHighlightResponse {
        request_id: i64,
        highlights: Vec<DocumentHighlight>,
    },
    /// Response to document symbol request
    DocumentSymbolResponse {
        request_id: i64,
        symbols: Vec<SymbolInformation>,
    },
    /// Response to folding range request
    FoldingRangeResponse {
        request_id: i64,
        ranges: Vec<FoldingRange>,
    },
    /// Response to prepare call hierarchy
    CallHierarchyPrepareResponse {
        request_id: i64,
        items: Vec<CallHierarchyItem>,
    },
    /// Response to incoming calls
    CallHierarchyIncomingResponse {
        request_id: i64,
        calls: Vec<CallHierarchyIncomingCall>,
    },
    /// Response to outgoing calls
    CallHierarchyOutgoingResponse {
        request_id: i64,
        calls: Vec<CallHierarchyOutgoingCall>,
    },
    /// Response to selection range request
    SelectionRangeResponse {
        request_id: i64,
        ranges: Vec<SelectionRange>,
    },
    /// Response to code lens request
    CodeLensResponse {
        request_id: i64,
        lenses: Vec<CodeLens>,
    },
    /// Response to codeLens/resolve
    CodeLensResolveResponse {
        _request_id: i64,
        lens: CodeLens,
    },
    /// Response to prepare type hierarchy
    TypeHierarchyPrepareResponse {
        request_id: i64,
        items: Vec<lsp_types::TypeHierarchyItem>,
    },
    /// Response to type hierarchy supertypes
    TypeHierarchySupertypesResponse {
        request_id: i64,
        items: Vec<lsp_types::TypeHierarchyItem>,
    },
    /// Response to type hierarchy subtypes
    TypeHierarchySubtypesResponse {
        request_id: i64,
        items: Vec<lsp_types::TypeHierarchyItem>,
    },
    /// Response to document links request
    DocumentLinkResponse {
        request_id: i64,
        links: Vec<DocumentLink>,
    },
    /// Response to completionItem/resolve
    CompletionResolveResponse {
        request_id: i64,
        item: Box<CompletionItem>,
    },
    /// Response to workspace/symbol
    WorkspaceSymbolResponse {
        request_id: i64,
        symbols: Vec<SymbolInformation>,
    },
    /// Response to textDocument/prepareRename
    PrepareRenameResponse {
        request_id: i64,
        range: Option<lsp_types::Range>,
        placeholder: Option<String>,
    },
    /// Response to workspace/willRenameFiles with edits to apply before rename
    WillRenameFilesResponse {
        request_id: i64,
        edit: Option<lsp_types::WorkspaceEdit>,
    },
    /// Server requests workspace configuration (workspace/configuration)
    WorkspaceConfigurationRequest {
        request_id: i64,
        items: Vec<lsp_types::ConfigurationItem>,
    },
    ServerRequest {
        request_id: Value,
        method: String,
        params: Value,
    },
}

impl LspManager {
    /// Poll all LSP clients for events and dispatch them.
    #[cfg(test)]
    pub(crate) fn poll(&mut self) {
        self.poll_until(std::time::Instant::now() + crate::ui_work::TURN_BUDGET);
    }

    pub(crate) fn poll_until(&mut self, deadline: std::time::Instant) {
        const MAX_EVENTS_PER_POLL: usize = 64;
        let mut processed = 0usize;
        let mut empty_clients = 0;
        // Rotate after each event: a diagnostic burst from one process must
        // not starve another process's completion or configuration request.
        while !self.clients.is_empty() && processed < MAX_EVENTS_PER_POLL {
            if processed > 0 && std::time::Instant::now() >= deadline { break; }
            let client_idx = self.next_poll_client % self.clients.len();
            self.next_poll_client = (client_idx + 1) % self.clients.len();
            let Ok(event) = self.clients[client_idx].event_rx.try_recv() else {
                empty_clients += 1;
                if empty_clients >= self.clients.len() { break; }
                continue;
            };
            empty_clients = 0;
            processed += 1;
                match event {
                    LspEvent::Initialized { capabilities } => {
                        self.clients[client_idx].initialized = true;
                        if let Some(caps) = *capabilities {
                            // Extract semantic token legend if server supports it
                            if let Some(ref provider) = caps.semantic_tokens_provider {
                                let legend = match provider {
                                    SemanticTokensServerCapabilities::SemanticTokensOptions(opts) => Some(opts.legend.clone()),
                                    SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(opts) => Some(opts.semantic_tokens_options.legend.clone()),
                                };
                                if let Some(legend) = legend {
                                    self.semantic_token_legend = Some(legend);
                                }
                            }
                            // Extract on-type formatting trigger characters
                            if let Some(ref otf) = caps.document_on_type_formatting_provider {
                                if !self.on_type_trigger_chars.contains(&otf.first_trigger_character) {
                                    self.on_type_trigger_chars.push(otf.first_trigger_character.clone());
                                }
                                if let Some(ref more) = otf.more_trigger_character {
                                    for ch in more {
                                        if !self.on_type_trigger_chars.contains(ch) {
                                            self.on_type_trigger_chars.push(ch.clone());
                                        }
                                    }
                                }
                            }
                            self.clients[client_idx].install_capabilities(caps);
                        }
                        let _ = self.clients[client_idx].send_initialized(); // Error OK: LSP notification is best-effort, server may be down
                    }
                    LspEvent::Diagnostics { uri, version, diagnostics } => {
                        if let Ok(path) = uri.to_file_path() {
                            self.receive_diagnostics(path, version, diagnostics, std::time::Instant::now());
                        }
                    }
                    LspEvent::CompletionResponse { request_id, items } => {
                        if self.active_completion_id == Some(request_id) {
                            self.cancel_completion_resolve();
                            self.completion_resolved_items.clear();
                            self.completion_items = items;
                            self.completion_revision = self.completion_revision.wrapping_add(1);
                            self.completion_response_ready = true;
                        }
                    }
                    LspEvent::CompletionResolveResponse { request_id, item } => {
                        self.finish_completion_resolve(request_id, Some(*item));
                    }
                    LspEvent::WorkspaceSymbolResponse { request_id, symbols } => {
                        if self.active_workspace_symbol_id == Some(request_id) {
                            self.workspace_symbols = symbols;
                            self.workspace_symbols_dirty = true;
                        }
                    }
                    LspEvent::HoverResponse { request_id, contents } => {
                        if self.active_hover_id == Some(request_id) {
                            self.active_hover_id = None;
                            self.hover_content = contents;
                        }
                    }
                    LspEvent::DefinitionResponse { request_id, locations } => {
                        if self.active_definition_link_id == Some(request_id) {
                            self.definition_link_available = !locations.is_empty();
                        } else if self.active_references_id == Some(request_id) {
                            // References response (same format as definition)
                            self.reference_locations = locations;
                            self.references_dirty = true;
                        } else if self.active_definition_id == Some(request_id)
                            || self.active_type_definition_id == Some(request_id)
                            || self.active_implementation_id == Some(request_id)
                            || self.active_declaration_id == Some(request_id)
                        {
                            self.definition_locations = locations;
                            self.definition_dirty = true;
                        }
                    }
                    LspEvent::SignatureHelpResponse { request_id, signatures, active_signature, active_parameter } => {
                        if self.active_signature_id == Some(request_id) {
                            self.signature_revision = self.signature_revision.wrapping_add(1);
                            self.signature_help = (!signatures.is_empty()).then_some(LspSignatureHelp {
                                signatures,
                                active_signature,
                                active_parameter,
                            });
                        }
                    }
                    LspEvent::CodeActionResponse { request_id, actions } => {
                        if self.active_code_action_id == Some(request_id) {
                            self.code_actions = actions;
                        }
                    }
                    LspEvent::FormatResponse { request_id, edits } => {
                        if self.active_format_id == Some(request_id) {
                            self.active_format_id = None;
                            self.format_edits = edits;
                            self.format_dirty = true;
                        }
                    }
                    LspEvent::RenameResponse { request_id, edit } => {
                        if self.active_rename_id == Some(request_id) {
                            self.rename_edit = edit;
                        }
                    }
                    LspEvent::PrepareRenameResponse { request_id, range, placeholder } => {
                        if self.active_prepare_rename_id == Some(request_id) {
                            self.prepare_rename_result = Some((range, placeholder));
                        }
                    }
                    LspEvent::WillRenameFilesResponse { request_id, edit } => {
                        if self.active_will_rename_id == Some(request_id) {
                            self.will_rename_edit = edit;
                            self.active_will_rename_id = None;
                        }
                    }
                    LspEvent::ServerError(_) => {
                        if self.active_format_id.is_some_and(|id| self.clients[client_idx].has_pending_request(id))
                        {
                            self.active_format_id = None;
                            self.format_edits.clear();
                            self.format_error = Some("The language server stopped before formatting finished. Restart it in Language Servers settings.".into());
                            self.format_dirty = true;
                        }
                        self.clients[client_idx].server_dead = true;
                    }
                    LspEvent::RequestError { request_id, method, message } => {
                        if method == "completionItem/resolve" {
                            self.finish_completion_resolve(request_id, None);
                        }
                        if self.active_format_id == Some(request_id)
                            && matches!(method.as_str(), "textDocument/formatting" | "textDocument/rangeFormatting")
                        {
                            self.format_error = Some(message);
                        }
                        if self.active_definition_link_id == Some(request_id) {
                            self.active_definition_link_id = None;
                            self.definition_link_available = false;
                        } else if self.finish_failed_request(client_idx, request_id, &method) || method == "workspace/executeCommand" {
                            self.request_errors.push(method);
                        }
                    }
                    LspEvent::InlayHintResponse { request_id, hints } => {
                        if self.active_inlay_hint_id == Some(request_id)
                            && self
                                .inlay_hint_request
                                .as_ref()
                                .is_some_and(|request| request.client_idx == client_idx)
                        {
                            self.active_inlay_hint_id = None;
                            self.inlay_hint_response_target = self.inlay_hint_request.clone();
                            self.inlay_hints = hints;
                            self.inlay_hints_dirty = true;
                        }
                    }
                    LspEvent::SemanticTokensResponse { request_id, tokens } => {
                        if self.active_semantic_tokens_id == Some(request_id) {
                            self.semantic_tokens_response = Some(tokens);
                            self.semantic_tokens_dirty = true;
                        }
                    }
                    LspEvent::DocumentHighlightResponse { request_id, highlights } => {
                        if self.active_highlight_id == Some(request_id) {
                            self.active_highlight_id = None;
                            self.document_highlights = highlights;
                            self.document_highlights_dirty = true;
                        }
                    }
                    LspEvent::DocumentSymbolResponse {
                        request_id,
                        symbols,
                    } => {
                        if self.active_document_symbol_id == Some(request_id)
                            && self
                                .document_symbol_request
                                .as_ref()
                                .is_some_and(|request| request.client_idx == client_idx)
                        {
                            self.active_document_symbol_id = None;
                            self.document_symbol_response_target =
                                self.document_symbol_request.clone();
                            self.document_symbols = symbols;
                            self.document_symbols_dirty = true;
                        }
                    }
                    LspEvent::FoldingRangeResponse { request_id, ranges } => {
                        if self.active_folding_range_id == Some(request_id)
                            && self
                                .document_symbol_request
                                .as_ref()
                                .is_none_or(|request| request.client_idx == client_idx)
                        {
                            self.active_folding_range_id = None;
                            self.folding_ranges = ranges;
                            self.folding_ranges_dirty = true;
                        }
                    }
                    LspEvent::CallHierarchyPrepareResponse { request_id, items } => {
                        if self.active_call_hierarchy_id == Some(request_id) {
                            self.call_hierarchy_items = items;
                            self.call_hierarchy_dirty = true;
                        }
                    }
                    LspEvent::CallHierarchyIncomingResponse { request_id, calls } => {
                        if self.active_incoming_calls_id == Some(request_id) {
                            self.call_hierarchy_incoming = calls;
                            self.call_hierarchy_incoming_dirty = true;
                        }
                    }
                    LspEvent::CallHierarchyOutgoingResponse { request_id, calls } => {
                        if self.active_outgoing_calls_id == Some(request_id) {
                            self.call_hierarchy_outgoing = calls;
                            self.call_hierarchy_outgoing_dirty = true;
                        }
                    }
                    LspEvent::SelectionRangeResponse { request_id, ranges } => {
                        if self.active_selection_range_id == Some(request_id)
                            && self.active_selection_range_client == Some(client_idx)
                        {
                            self.active_selection_range_id = None;
                            self.active_selection_range_client = None;
                            self.selection_ranges = ranges;
                            self.selection_ranges_dirty = true;
                        }
                    }
                    LspEvent::CodeLensResponse { request_id, lenses } => {
                        if self.active_code_lens_id == Some(request_id)
                            && self
                                .document_symbol_request
                                .as_ref()
                                .is_none_or(|request| request.client_idx == client_idx)
                        {
                            self.active_code_lens_id = None;
                            self.code_lenses = lenses;
                            self.code_lenses_dirty = true;
                        }
                    }
                    LspEvent::CodeLensResolveResponse { _request_id: _, lens } => {
                        self.code_lens_resolve_results.push(lens);
                        self.code_lens_resolve_dirty = true;
                    }
                    LspEvent::TypeHierarchyPrepareResponse { request_id, items } => {
                        if self.active_type_hierarchy_id == Some(request_id) {
                            self.type_hierarchy_items = items;
                            self.type_hierarchy_dirty = true;
                        }
                    }
                    LspEvent::TypeHierarchySupertypesResponse { request_id, items } => {
                        if self.active_supertypes_id == Some(request_id) {
                            self.type_hierarchy_supertypes = items;
                            self.type_hierarchy_supertypes_dirty = true;
                        }
                    }
                    LspEvent::TypeHierarchySubtypesResponse { request_id, items } => {
                        if self.active_subtypes_id == Some(request_id) {
                            self.type_hierarchy_subtypes = items;
                            self.type_hierarchy_subtypes_dirty = true;
                        }
                    }
                    LspEvent::DocumentLinkResponse { request_id, links } => {
                        if self.active_document_links_id == Some(request_id)
                            && self
                                .document_symbol_request
                                .as_ref()
                                .is_none_or(|request| request.client_idx == client_idx)
                        {
                            self.active_document_links_id = None;
                            self.document_links = links;
                            self.document_links_dirty = true;
                        }
                    }
                    LspEvent::OnTypeFormattingResponse { request_id, edits } => {
                        if self.active_on_type_format_id == Some(request_id) {
                            self.on_type_format_edits = edits;
                            self.on_type_format_dirty = true;
                        }
                    }
                    LspEvent::WorkspaceConfigurationRequest { request_id, items } => {
                        let settings = items.iter().map(|item| self.clients[client_idx].configuration(item.section.as_deref())).collect();
                        let _ = self.clients[client_idx].respond_configuration(request_id, settings);
                    }
                    LspEvent::ServerRequest { request_id, method, params } => {
                        self.handle_server_request(client_idx, request_id, &method, params);
                    }
                }
        }

        // A bounded drain keeps an LSP burst from monopolizing input. If the
        // cap was reached, explicitly re-wake so already-queued events cannot
        // be stranded now that the main loop has no fallback poll timer.
        if processed >= MAX_EVENTS_PER_POLL || (processed > 0 && std::time::Instant::now() >= deadline) {
            self.wake.notify();
        }

        // Reader/writer lifecycle events replace per-frame child.try_wait().
        // LspClient's graceful Drop may wait briefly for a stubborn server, so
        // move dead clients to a reaper instead of dropping them on the UI.
        if self.clients.iter().any(|client| client.server_dead) {
            // Client indices change below. Retire handles before rebuilding,
            // so a surviving server cannot inherit another server's request ID.
            self.cancel_inlay_hint_request();
            self.cancel_document_symbol_request();
            self.cancel_document_highlight();
            let mut live = Vec::with_capacity(self.clients.len());
            for client in self.clients.drain(..) {
                if client.server_dead {
                    self.startup_errors.insert(client.language.clone(),
                        "Server stopped. Check its command and arguments, then Restart Servers.".into());
                    let _ = std::thread::Builder::new()
                        .name("lsp-reaper".to_string())
                        .spawn(move || drop(client));
                } else {
                    live.push(client);
                }
            }
            self.clients = live;
        }
        self.publish_pending_diagnostics(std::time::Instant::now());
    }
}
