//! Capability checks are prepared once per server, never serialized per frame.
use super::*;

const PROVIDERS: &[(&str, &str)] = &[
    ("textDocument/completion", "/completionProvider"),
    (
        "completionItem/resolve",
        "/completionProvider/resolveProvider",
    ),
    ("textDocument/hover", "/hoverProvider"),
    ("textDocument/definition", "/definitionProvider"),
    ("textDocument/declaration", "/declarationProvider"),
    ("textDocument/typeDefinition", "/typeDefinitionProvider"),
    ("textDocument/implementation", "/implementationProvider"),
    ("textDocument/references", "/referencesProvider"),
    ("textDocument/signatureHelp", "/signatureHelpProvider"),
    ("textDocument/rename", "/renameProvider"),
    (
        "textDocument/prepareRename",
        "/renameProvider/prepareProvider",
    ),
    ("textDocument/formatting", "/documentFormattingProvider"),
    (
        "textDocument/rangeFormatting",
        "/documentRangeFormattingProvider",
    ),
    (
        "textDocument/onTypeFormatting",
        "/documentOnTypeFormattingProvider",
    ),
    ("textDocument/codeAction", "/codeActionProvider"),
    ("textDocument/inlayHint", "/inlayHintProvider"),
    (
        "textDocument/documentHighlight",
        "/documentHighlightProvider",
    ),
    ("textDocument/documentSymbol", "/documentSymbolProvider"),
    ("textDocument/foldingRange", "/foldingRangeProvider"),
    ("textDocument/selectionRange", "/selectionRangeProvider"),
    (
        "textDocument/semanticTokens/full",
        "/semanticTokensProvider/full",
    ),
    ("textDocument/codeLens", "/codeLensProvider"),
    ("codeLens/resolve", "/codeLensProvider/resolveProvider"),
    ("textDocument/documentLink", "/documentLinkProvider"),
    (
        "textDocument/prepareCallHierarchy",
        "/callHierarchyProvider",
    ),
    ("callHierarchy/incomingCalls", "/callHierarchyProvider"),
    ("callHierarchy/outgoingCalls", "/callHierarchyProvider"),
    (
        "textDocument/prepareTypeHierarchy",
        "/typeHierarchyProvider",
    ),
    ("typeHierarchy/supertypes", "/typeHierarchyProvider"),
    ("typeHierarchy/subtypes", "/typeHierarchyProvider"),
    ("workspace/symbol", "/workspaceSymbolProvider"),
    ("workspace/executeCommand", "/executeCommandProvider"),
    (
        "workspace/willRenameFiles",
        "/workspace/fileOperations/willRename",
    ),
];

impl LspClient {
    pub(crate) fn install_capabilities(&mut self, capabilities: ServerCapabilities) {
        let sync = match &capabilities.text_document_sync {
            Some(TextDocumentSyncCapability::Kind(kind)) => *kind,
            Some(TextDocumentSyncCapability::Options(options)) => options.change.unwrap_or(TextDocumentSyncKind::NONE),
            None => TextDocumentSyncKind::NONE,
        };
        let _ = self.outbound_tx.send(LspOutbound::SyncKind(sync));
        let json = serde_json::to_value(&capabilities).unwrap_or_default();
        self.supported_requests = Some(
            PROVIDERS
                .iter()
                .filter_map(|&(method, pointer)| {
                    json.pointer(pointer)
                        .filter(|value| !value.is_null() && **value != Value::Bool(false))
                        .map(|_| method)
                })
                .collect(),
        );
        self.capabilities = Some(capabilities);
    }

    pub(crate) fn supports_request(&self, method: &str) -> bool {
        self.supported_requests.as_ref().is_none_or(|supported| {
            !PROVIDERS.iter().any(|&(candidate, _)| candidate == method)
                || supported.contains(method)
        })
    }

    pub(super) fn document_language(&self, path: &Path) -> String {
        if let Some((_, language)) = crate::file_associations::resolve(&self.file_associations, path) {
            return language.into();
        }
        if matches!(self.language.as_str(), "c" | "cpp" | "c++") {
            return match path.extension().and_then(|extension| extension.to_str()) {
                Some("c") => "c",
                Some("C" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh") => "cpp",
                Some("m") => "objective-c",
                Some("mm") => "objective-cpp",
                _ => &self.language,
            }
            .into();
        }
        self.language.clone()
    }
}

impl LspManager {
    pub(crate) fn code_action_diagnostics(&self, path: &Path, line: u32) -> Vec<Diagnostic> {
        self.current_diagnostic_context(path)
            .into_iter()
            .flatten()
            .filter(|diagnostic| {
                diagnostic.range.start.line <= line && diagnostic.range.end.line >= line
            })
            .cloned()
            .collect()
    }
    pub(crate) fn semantic_legend_for_file(
        &self,
        path: Option<&Path>,
    ) -> Option<&SemanticTokensLegend> {
        let client = self
            .clients
            .iter()
            .find(|client| path.is_some_and(|path| client.handles_file(path)))?;
        match client
            .capabilities
            .as_ref()
            .and_then(|caps| caps.semantic_tokens_provider.as_ref())
        {
            Some(SemanticTokensServerCapabilities::SemanticTokensOptions(options)) => {
                Some(&options.legend)
            }
            Some(SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(options)) => {
                Some(&options.semantic_tokens_options.legend)
            }
            None if client.capabilities.is_none() => self.semantic_token_legend.as_ref(),
            None => None,
        }
    }
}
