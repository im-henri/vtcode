//! LSP request-building methods for `LspClient`.
//!
//! Each method constructs the appropriate `lsp_types` params and delegates
//! to `LspClient::send_request` (defined in `lsp.rs`).

use std::path::Path;

use lsp_types::*;

use crate::lsp::{CodeActionParams, FormatRangeParams, LspClient, OnTypeFormattingParams};

impl LspClient {
    // ── Language Features ─────────────────────────────────────────

    /// Request completions at a position
    pub(crate) fn request_completion(&mut self, path: &Path, line: u32, col: u32, prefix: &str) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        self.send_request_with_context(crate::lsp::PendingRequest {
            method: "textDocument/completion".into(),
            completion_prefix: Some(prefix.into()),
        }, Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request completion item resolve (fetch detailed docs / additional edits)
    pub(crate) fn request_completion_resolve(&mut self, item: &CompletionItem) -> Result<i64, String> {
        self.send_request("completionItem/resolve", Some(serde_json::to_value(item).map_err(|e| e.to_string())?))
    }

    /// Request workspace symbols matching a query
    pub(crate) fn request_workspace_symbol(&mut self, query: &str) -> Result<i64, String> {
        let params = WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("workspace/symbol", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request hover information at a position
    pub(crate) fn request_hover(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
        };
        self.send_request("textDocument/hover", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request go-to-definition
    pub(crate) fn request_definition(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/definition", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request go-to-type-definition
    pub(crate) fn request_type_definition(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/typeDefinition", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request go-to-implementation
    pub(crate) fn request_implementation(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/implementation", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request go-to-declaration
    pub(crate) fn request_declaration(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/declaration", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request find all references
    pub(crate) fn request_references(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: ReferenceContext {
                include_declaration: true,
            },
        };
        self.send_request("textDocument/references", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request signature help at a position
    pub(crate) fn request_signature_help(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = SignatureHelpParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            context: None,
        };
        self.send_request("textDocument/signatureHelp", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request code actions at a range
    pub(crate) fn request_code_action(&mut self, p: &CodeActionParams) -> Result<i64, String> {
        let uri = Url::from_file_path(p.path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range: Range {
                start: Position { line: p.start_line, character: p.start_col },
                end: Position { line: p.end_line, character: p.end_col },
            },
            context: CodeActionContext {
                diagnostics: p.diagnostics.clone(),
                only: None,
                trigger_kind: Some(CodeActionTriggerKind::INVOKED),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/codeAction", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request document formatting
    pub(crate) fn request_format_document(&mut self, path: &Path, tab_size: u32, insert_spaces: bool) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::DocumentFormattingParams {
            text_document: TextDocumentIdentifier { uri },
            options: lsp_types::FormattingOptions {
                tab_size,
                insert_spaces,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };
        self.send_request("textDocument/formatting", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request range formatting (format selection)
    pub(crate) fn request_format_range(&mut self, p: &FormatRangeParams) -> Result<i64, String> {
        let uri = Url::from_file_path(p.path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::DocumentRangeFormattingParams {
            text_document: TextDocumentIdentifier { uri },
            range: lsp_types::Range {
                start: Position { line: p.start_line, character: p.start_col },
                end: Position { line: p.end_line, character: p.end_col },
            },
            options: lsp_types::FormattingOptions {
                tab_size: p.tab_size,
                insert_spaces: p.insert_spaces,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        };
        self.send_request("textDocument/rangeFormatting", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request on-type formatting after typing a trigger character
    pub(crate) fn request_on_type_formatting(&mut self, p: &OnTypeFormattingParams) -> Result<i64, String> {
        let uri = Url::from_file_path(p.path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::DocumentOnTypeFormattingParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line: p.line, character: p.col },
            },
            ch: p.ch.to_string(),
            options: lsp_types::FormattingOptions {
                tab_size: p.tab_size,
                insert_spaces: p.insert_spaces,
                ..Default::default()
            },
        };
        self.send_request("textDocument/onTypeFormatting", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request prepare rename to validate rename position and get placeholder
    pub(crate) fn request_prepare_rename(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character: col },
        };
        self.send_request("textDocument/prepareRename", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    pub(crate) fn request_rename(&mut self, path: &Path, line: u32, col: u32, new_name: &str) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::RenameParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            new_name: new_name.to_string(),
            work_done_progress_params: Default::default(),
        };
        self.send_request("textDocument/rename", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request inlay hints for a range
    pub(crate) fn request_inlay_hints(&mut self, path: &Path, start_line: u32, end_line: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::InlayHintParams {
            text_document: TextDocumentIdentifier { uri },
            range: Range {
                start: Position { line: start_line, character: 0 },
                end: Position { line: end_line, character: 0 },
            },
            work_done_progress_params: Default::default(),
        };
        self.send_request("textDocument/inlayHint", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request semantic tokens for the full document
    pub(crate) fn request_semantic_tokens_full(&mut self, path: &Path) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = SemanticTokensParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/semanticTokens/full", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request document highlights for symbol under cursor
    pub(crate) fn request_document_highlight(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::DocumentHighlightParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/documentHighlight", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request document symbols for outline
    pub(crate) fn request_document_symbol(&mut self, path: &Path) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::DocumentSymbolParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/documentSymbol", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request folding ranges for a file
    pub(crate) fn request_folding_range(&mut self, path: &Path) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::FoldingRangeParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/foldingRange", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request prepare call hierarchy
    pub(crate) fn request_prepare_call_hierarchy(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::CallHierarchyPrepareParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
        };
        self.send_request("textDocument/prepareCallHierarchy", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request incoming calls for a call hierarchy item
    pub(crate) fn request_incoming_calls(&mut self, item: &CallHierarchyItem) -> Result<i64, String> {
        let params = lsp_types::CallHierarchyIncomingCallsParams {
            item: item.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("callHierarchy/incomingCalls", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request outgoing calls for a call hierarchy item
    pub(crate) fn request_outgoing_calls(&mut self, item: &CallHierarchyItem) -> Result<i64, String> {
        let params = lsp_types::CallHierarchyOutgoingCallsParams {
            item: item.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("callHierarchy/outgoingCalls", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request selection ranges for given positions
    pub(crate) fn request_selection_range(&mut self, path: &Path, positions: Vec<Position>) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::SelectionRangeParams {
            text_document: TextDocumentIdentifier { uri },
            positions,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/selectionRange", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request code lenses for a file
    pub(crate) fn request_code_lens(&mut self, path: &Path) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::CodeLensParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/codeLens", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Resolve a code lens to get its command details
    pub(crate) fn request_code_lens_resolve(&mut self, lens: &CodeLens) -> Result<i64, String> {
        self.send_request("codeLens/resolve", Some(serde_json::to_value(lens).map_err(|e| e.to_string())?))
    }

    /// Request document links for a file
    pub(crate) fn request_document_links(&mut self, path: &Path) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::DocumentLinkParams {
            text_document: TextDocumentIdentifier { uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("textDocument/documentLink", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request prepare type hierarchy
    pub(crate) fn request_prepare_type_hierarchy(&mut self, path: &Path, line: u32, col: u32) -> Result<i64, String> {
        let uri = Url::from_file_path(path).map_err(|_| "Invalid file path")?;
        let params = lsp_types::TypeHierarchyPrepareParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character: col },
            },
            work_done_progress_params: Default::default(),
        };
        self.send_request("textDocument/prepareTypeHierarchy", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request supertypes for a type hierarchy item
    pub(crate) fn request_supertypes(&mut self, item: &lsp_types::TypeHierarchyItem) -> Result<i64, String> {
        let params = lsp_types::TypeHierarchySupertypesParams {
            item: item.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("typeHierarchy/supertypes", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }

    /// Request subtypes for a type hierarchy item
    pub(crate) fn request_subtypes(&mut self, item: &lsp_types::TypeHierarchyItem) -> Result<i64, String> {
        let params = lsp_types::TypeHierarchySubtypesParams {
            item: item.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        self.send_request("typeHierarchy/subtypes", Some(serde_json::to_value(&params).map_err(|e| e.to_string())?))
    }
}
