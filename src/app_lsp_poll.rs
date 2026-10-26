use crate::app::App;

#[cfg(test)]
#[path = "app_lsp_hover_tests.rs"]
mod hover_tests;

#[cfg(test)]
#[path = "app_lsp_selection_tests.rs"]
mod selection_tests;

pub(crate) const LSP_EDIT_QUIET_PERIOD: std::time::Duration =
    std::time::Duration::from_millis(750);

fn inlay_range(
    editor: &crate::editor::Editor,
    previous: Option<&crate::lsp::DocumentFeedbackRequest>,
) -> (u32, u32) {
    let start = editor.scroll_offset.min(editor.rope.len_lines()) as u32;
    let height = editor.viewport_height.max(1);
    let end = editor
        .visible_line_at(editor.scroll_offset, height)
        .unwrap_or(editor.rope.len_lines()) as u32;
    // Reuse both completed and in-flight requests while they cover the visible
    // rows. This avoids cancel/reissue storms during ordinary wheel scrolling.
    if let Some(request) = previous
        && editor
            .file_path
            .as_deref()
            .is_some_and(|path| request.matches(path, editor.content_version, request.range))
        && let Some((requested_start, requested_end)) = request.range
        && requested_start <= start
        && requested_end >= end
    {
        return (requested_start, requested_end);
    }
    let margin = height.max(50);
    (
        editor.scroll_offset.saturating_sub(margin) as u32,
        editor
            .visible_line_at(editor.scroll_offset, height.saturating_add(margin))
            .unwrap_or(editor.rope.len_lines()) as u32,
    )
}

/// Passive requests observe one current snapshot. During typing its latest
/// version/viewport stays in the editor rather than becoming queued LSP work.
fn request_passive_document_feedback(app: &mut App, historical: bool) {
    if historical {
        return;
    }
    let Some(editor) = app
        .tabs
        .get(app.active_tab)
        .map(|tab| &tab.editor)
        .filter(|editor| !editor.large_file_mode && !editor.bg_loading)
    else {
        return;
    };
    let Some(path) = editor.file_path.as_deref() else {
        return;
    };
    let version = editor.content_version;
    if app.lsp_manager.last_notified_version.get(path) != Some(&version) {
        return;
    }
    let Some(client) = app.lsp_manager.clients.iter().find(|client| {
        client.initialized && !client.server_dead && client.handles_file(path) && client.is_file_open(path)
    }) else { return; };
    let folding_supported = client.supports_request("textDocument/foldingRange");
    let lenses_supported = client.supports_request("textDocument/codeLens");
    let links_supported = client.supports_request("textDocument/documentLink");
    let quiet = app.last_edit_time.elapsed() >= LSP_EDIT_QUIET_PERIOD;
    let new_document = app
        .lsp_manager
        .document_symbol_request
        .as_ref()
        .is_none_or(|request| !request.matches(path, version, None));
    if app.show_inlay_hints && (quiet || app.lsp_inlay_hint_last_version == version) {
        let (start, end) = inlay_range(editor, app.lsp_manager.inlay_hint_request.as_ref());
        if app.lsp_manager.request_inlay_hints(path, start, end) {
            app.lsp_inlay_hint_last_scroll = editor.scroll_offset;
            app.lsp_inlay_hint_last_version = version;
        }
    }
    if quiet && app.lsp_manager.request_document_symbol(path) {
        app.lsp_outline_last_version = version;
    }
    if quiet {
        if (new_document || app.lsp_folding_last_version != version)
            && (!folding_supported || app.lsp_manager.request_folding_range(path))
        {
            app.lsp_folding_last_version = version;
        }
        if app.show_code_lenses
            && (new_document || app.lsp_code_lens_last_version != version)
            && (!lenses_supported || app.lsp_manager.request_code_lens(path))
        {
            app.lsp_code_lens_last_version = version;
        }
        if (new_document || app.lsp_document_links_last_version != version)
            && (!links_supported || app.lsp_manager.request_document_links(path))
        {
            app.lsp_document_links_last_version = version;
        }
    }
}

fn feedback_response_is_current(
    app: &App,
    target: &crate::lsp::DocumentFeedbackRequest,
    historical: bool,
    inlay: bool,
) -> bool {
    !historical
        && app.editor().is_some_and(|editor| {
            !editor.large_file_mode
                && !editor.bg_loading
                && editor.file_path.as_deref().is_some_and(|path| {
                    target.matches(
                        path,
                        editor.content_version,
                        inlay.then(|| inlay_range(editor, app.lsp_manager.inlay_hint_request.as_ref())),
                    )
                })
        })
}

/// Apply LSP text edits to a file, returning the number of edits applied.
#[cfg(test)]
pub(crate) fn apply_lsp_text_edits(
    app: &mut App,
    path: &std::path::Path,
    edits: Vec<lsp_types::TextEdit>,
) -> usize {
    if edits.is_empty() { return 0; }
    app.open_file(path);
    let Some(editor) = app.editor_mut().filter(|editor| editor.file_path.as_deref() == Some(path)) else { return 0; };
    crate::app_lsp_edits::apply(editor, &edits).unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn poll_lsp(app: &mut App, needs_redraw: &mut bool) {
    poll_lsp_until(app, needs_redraw, std::time::Instant::now() + crate::ui_work::TURN_BUDGET);
}

pub(crate) fn poll_lsp_until(app: &mut App, needs_redraw: &mut bool, deadline: std::time::Instant) {
    *needs_redraw |= app.poll_external_format();
    *needs_redraw |= app.flush_lsp_document_syncs();
    *needs_redraw |= app.refresh_lsp_link_hover();
    if app.lsp_selection_range_source.is_some() && !app.lsp_selection_context_current() {
        app.clear_lsp_selection_context();
    }
    app.dispatch_lsp_suggestions();
    let historical = app.tabs.get(app.active_tab).is_some_and(|tab| {
        tab.kind == crate::app::TabKind::GitRevision
            || app.diff_tabs.blame_files_for_tab(tab).is_some()
    });
    // Retire obsolete IDs before accepting already-buffered replies. This also
    // handles tab switches, scrolling, closed files and disabled hint settings.
    let editor = app
        .tabs
        .get(app.active_tab)
        .map(|tab| &tab.editor)
        .filter(|editor| !historical && !editor.large_file_mode && !editor.bg_loading);
    let document = editor.and_then(|editor| {
        editor
            .file_path
            .as_deref()
            .map(|path| (path, editor.content_version))
    });
    let range = editor.filter(|_| app.show_inlay_hints && app.lsp_manager.inlay_hint_request.is_some())
        .map(|editor| inlay_range(editor, app.lsp_manager.inlay_hint_request.as_ref()));
    let highlight_target = editor.filter(|editor| app.lsp_highlight_last_cursor == (editor.cursor_line, editor.cursor_col)).and_then(|editor| {
        let (_, (line, col)) = app.lsp_manager.highlight_request.as_ref()?;
        Some((editor.file_path.as_deref()?, editor.content_version, *line, *col))
    });
    app.lsp_manager.retain_document_highlight(highlight_target);
    if app.lsp_inlay_hint_last_scroll == usize::MAX {
        app.lsp_manager.cancel_inlay_hint_request();
    }
    app.lsp_manager.retain_document_feedback(document, range);
    // Poll LSP servers for events (diagnostics, completions, hover, etc.)
    let connection_state = (app.lsp_manager.clients.len(), app.lsp_manager.startup_errors.len(),
        app.lsp_manager.clients.iter().filter(|client| client.initialized).count());
    let link_available = app.lsp_manager.definition_link_available;
    let diagnostic_version = app.lsp_manager.diagnostics_version;
    let had_hover_content = app.lsp_manager.hover_content.is_some();
    app.lsp_manager.poll_until(deadline);
    *needs_redraw |= app.poll_lsp_format_request();
    *needs_redraw |= link_available != app.lsp_manager.definition_link_available;
    *needs_redraw |= diagnostic_version != app.lsp_manager.diagnostics_version;
    *needs_redraw |= app.lsp_hover_active
        && had_hover_content != app.lsp_manager.hover_content.is_some();
    app.dispatch_lsp_suggestions();
    *needs_redraw |= connection_state != (app.lsp_manager.clients.len(), app.lsp_manager.startup_errors.len(),
        app.lsp_manager.clients.iter().filter(|client| client.initialized).count());
    if std::mem::take(&mut app.lsp_manager.completion_response_ready) {
        app.finish_lsp_completion_response();
        *needs_redraw |= app.lsp_completion_active;
    }
    for method in std::mem::take(&mut app.lsp_manager.request_errors) {
        match method.as_str() {
            "textDocument/definition"
            | "textDocument/typeDefinition"
            | "textDocument/implementation"
            | "textDocument/declaration" => app.lsp_goto_pending = false,
            "textDocument/references" => app.lsp_references_pending = false,
            "textDocument/rename" => app.lsp_rename_pending = false,
            "textDocument/prepareRename" => app.lsp_prepare_rename_pending = false,
            "textDocument/completion" => {
                app.completion_ui.status = Some("Unavailable — Ctrl+Space to retry");
                if !app.completion_ui.explicit { app.lsp_completion_active = false; }
                *needs_redraw = true;
            }
            "textDocument/hover" => app.lsp_hover_active = false,
            "textDocument/signatureHelp" => app.lsp_signature_active = false,
            "textDocument/codeAction" => app.lsp_code_action_active = false,
            "textDocument/onTypeFormatting" => app.lsp_on_type_format_target = None,
            "textDocument/semanticTokens/full" => app.lsp_semantic_tokens_request_target = None,
            "workspace/executeCommand" => app.notify_error("The language server could not execute the code action"),
            _ => {}
        }
    }
    let resolved = std::mem::take(&mut app.lsp_manager.completion_resolve_dirty);
    *needs_redraw |= app.lsp_completion_active && resolved;
    if app.lsp_completion_active {
        if let Some(path) = app.tabs.get(app.active_tab).and_then(|tab| tab.editor.file_path.as_deref()) {
            app.lsp_manager.request_completion_resolve(path, app.lsp_completion_selected);
        }
    } else {
        app.lsp_manager.cancel_completion_resolve();
    }
    // Notify LSP of content changes in the active editor (skip large files)
    if !historical && !app.lsp_manager.configs.is_empty() && app.last_edit_time.elapsed() >= LSP_EDIT_QUIET_PERIOD
    {
        let pending_sync = app.editor().and_then(|editor| {
            if editor.large_file_mode {
                return None;
            }
            let path = editor.file_path.clone()?;
            let content_version = editor.content_version;
            let last_notified = app
                .lsp_manager
                .last_notified_version
                .get(&path)
                .copied()
                .unwrap_or(0);
            (content_version > last_notified).then(|| (path, content_version, editor.rope.clone()))
        });
        if let Some((path, content_version, rope)) = pending_sync {
            app.queue_lsp_document_sync(path, content_version, rope);
        }
    }
    // Arm the delay only when this file's hover provider is ready. Initialization
    // wakes the event loop, so a stationary pointer can start its delay then;
    // an early failed request must not consume the hover for that position.
    let hover_pos = (app.hover_enabled && !app.lsp_hover_suppressed && !historical
        && app.editor().is_some_and(|editor| !editor.large_file_mode && !editor.bg_loading
            && editor.file_path.as_deref().is_some_and(|path| {
                app.lsp_manager.clients.iter().any(|client| client.initialized
                    && !client.server_dead && client.handles_file(path)
                    && client.supports_request("textDocument/hover"))
            })))
        .then_some(app.hovered_editor_pos).flatten();
    if let Some((line, col)) = hover_pos {
        if app.lsp_hover_pending_pos != Some((line, col)) {
            *needs_redraw |= app.clear_lsp_hover();
            app.lsp_hover_pending_pos = Some((line, col));
            app.lsp_hover_request_time = Some(std::time::Instant::now());
        } else if app.lsp_hover_request_time.is_some_and(|time| {
            time.elapsed() >= std::time::Duration::from_millis(500)
        }) {
            app.lsp_hover_request_time = None;
            app.lsp_hover_line = line;
            app.lsp_hover_col = col;
            if let Some(editor) = app.editor()
                && let Some(path) = editor.file_path.clone()
            {
                let col = crate::lsp_positions::to_utf16(&editor.rope, line, col);
                let version = editor.content_version;
                let rope = editor.rope.clone();
                // Hover can become due before the passive edit quiet period.
                // Queue the current snapshot ahead of the request on the LSP
                // writer, otherwise the server may return no symbol at all.
                app.queue_lsp_document_sync(path.clone(), version, rope);
                *needs_redraw |= app.flush_lsp_document_syncs();
                app.lsp_hover_active = app.lsp_manager.request_hover(&path, line as u32, col);
            }
        }
    } else {
        *needs_redraw |= app.clear_lsp_hover();
    }
    request_passive_document_feedback(app, historical);
    // Process inlay hint responses
    if app.lsp_manager.inlay_hints_dirty {
        app.lsp_manager.inlay_hints_dirty = false;
        let hints = std::mem::take(&mut app.lsp_manager.inlay_hints);
        let is_current = app.show_inlay_hints
            && app
                .lsp_manager
                .inlay_hint_response_target
                .take()
                .is_some_and(|target| feedback_response_is_current(app, &target, historical, true));
        if is_current {
            let mut mapped: Vec<_> = hints
                .into_iter()
                .filter_map(|hint| {
                    let label = match &hint.label {
                        lsp_types::InlayHintLabel::String(s) => s.clone(),
                        lsp_types::InlayHintLabel::LabelParts(parts) => parts
                            .iter()
                            .map(|part| part.value.as_str())
                            .collect::<String>(),
                    };
                    (!label.is_empty()).then_some(crate::app::InlayHintDisplay::new(
                        hint.position.line,
                        app.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, hint.position.line as usize, hint.position.character) as u32),
                        label,
                        hint.kind,
                    ))
                })
                .collect();
            mapped.sort_unstable_by(|a, b| {
                (a.line, a.column, a.label.as_str(), a.color_kind_index()).cmp(&(
                    b.line,
                    b.column,
                    b.label.as_str(),
                    b.color_kind_index(),
                ))
            });
            if app.lsp_inlay_hints != mapped {
                app.lsp_inlay_hints = mapped;
                *needs_redraw = true;
            }
        }
    }
    // Request semantic tokens when content changes
    if app.show_semantic_tokens && !app.lsp_manager.clients.is_empty() {
        let is_large = historical || app.editor().is_some_and(|e| e.large_file_mode || e.bg_loading);
        if !is_large {
            if let Some(ed) = app.editor() {
                let cv = ed.content_version;
                if let Some(fp) = ed.file_path.as_ref() {
                    let document_is_synced = app
                        .lsp_manager
                        .last_notified_version
                        .get(fp)
                        .is_some_and(|version| *version == cv);
                    if cv != app.lsp_semantic_tokens_last_version
                        && document_is_synced
                        && app.last_edit_time.elapsed() >= LSP_EDIT_QUIET_PERIOD
                        && app.lsp_manager.clients.iter().any(|client| client.initialized
                            && client.handles_file(fp) && client.supports_request("textDocument/semanticTokens/full"))
                    {
                        let fp = fp.clone();
                        let request_target = app.editor_content_target();
                        if app.lsp_manager.request_semantic_tokens_full(&fp) {
                            app.lsp_semantic_tokens_last_version = cv;
                            app.lsp_semantic_tokens_request_target = request_target;
                        }
                    }
                }
            }
        }
    }
    // Process semantic token responses
    if app.lsp_manager.semantic_tokens_dirty {
        app.lsp_manager.semantic_tokens_dirty = false;
        let response = app.lsp_manager.semantic_tokens_response.take();
        let target = app.lsp_semantic_tokens_request_target.take();
        if let (Some(response), Some(target)) = (response, target) {
            if app.editor_content_target().as_ref() == Some(&target) {
                app.queue_lsp_semantic_decode(target, response.data);
            }
        }
    }
    // Request document highlights when cursor position changes
    if !app.lsp_manager.clients.is_empty() {
        let is_large = historical || app.editor().is_some_and(|e| e.large_file_mode || e.bg_loading);
        if !is_large {
            if let Some(ed) = app.editor() {
                let cl = ed.cursor_line;
                let cc = ed.cursor_col;
                let cv = ed.content_version;
                if let Some(fp) = ed.file_path.as_ref() {
                    let document_is_synced = app
                        .lsp_manager
                        .last_notified_version
                        .get(fp)
                        .is_some_and(|version| *version == cv);
                    if app.lsp_manager.highlight_request.is_none()
                        && document_is_synced
                        && app.last_edit_time.elapsed() >= LSP_EDIT_QUIET_PERIOD
                    {
                        let fp = fp.clone();
                        let col = crate::lsp_positions::to_utf16(&ed.rope, cl, cc);
                        if app.lsp_manager.request_document_highlight(&fp, cl as u32, col) {
                            app.lsp_highlight_last_cursor = (cl, cc);
                        }
                    }
                }
            }
        }
    }
    // Process document highlight responses
    if app.lsp_manager.document_highlights_dirty {
        app.lsp_manager.document_highlights_dirty = false;
        let highlights = std::mem::take(&mut app.lsp_manager.document_highlights);
        let mut mapped: Vec<_> = highlights
            .iter()
            .map(|highlight| {
                (
                    highlight.range.start.line,
                    app.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, highlight.range.start.line as usize, highlight.range.start.character) as u32),
                    highlight.range.end.line,
                    app.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, highlight.range.end.line as usize, highlight.range.end.character) as u32),
                )
            })
            .collect();
        mapped.sort_unstable();
        app.lsp_document_multiline_highlights = mapped
            .iter()
            .copied()
            .filter(|highlight| highlight.0 != highlight.2)
            .collect();
        app.lsp_document_highlights = mapped;
        *needs_redraw = true;
    }
    // Process document symbol responses for outline
    if app.lsp_manager.document_symbols_dirty {
        app.lsp_manager.document_symbols_dirty = false;
        let symbols = std::mem::take(&mut app.lsp_manager.document_symbols);
        let is_current = app
            .lsp_manager
            .document_symbol_response_target
            .take()
            .is_some_and(|target| feedback_response_is_current(app, &target, historical, false));
        if is_current {
            if !symbols.is_empty() {
                app.lsp_outline_available = true;
                app.source_symbol_work.cancel_outline();
                app.outline_items.clear();
                for sym in &symbols {
                    let kind = match sym.kind {
                        lsp_types::SymbolKind::FILE => "file",
                        lsp_types::SymbolKind::MODULE => "module",
                        lsp_types::SymbolKind::NAMESPACE => "namespace",
                        lsp_types::SymbolKind::PACKAGE => "module",
                        lsp_types::SymbolKind::CLASS => "class",
                        lsp_types::SymbolKind::METHOD => "function",
                        lsp_types::SymbolKind::PROPERTY => "property",
                        lsp_types::SymbolKind::FIELD => "field",
                        lsp_types::SymbolKind::CONSTRUCTOR => "function",
                        lsp_types::SymbolKind::ENUM => "enum",
                        lsp_types::SymbolKind::INTERFACE => "interface",
                        lsp_types::SymbolKind::FUNCTION => "function",
                        lsp_types::SymbolKind::VARIABLE => "variable",
                        lsp_types::SymbolKind::CONSTANT => "constant",
                        lsp_types::SymbolKind::STRING => "string",
                        lsp_types::SymbolKind::NUMBER => "number",
                        lsp_types::SymbolKind::BOOLEAN => "boolean",
                        lsp_types::SymbolKind::ARRAY => "array",
                        lsp_types::SymbolKind::OBJECT => "object",
                        lsp_types::SymbolKind::KEY => "key",
                        lsp_types::SymbolKind::NULL => "null",
                        lsp_types::SymbolKind::ENUM_MEMBER => "enum",
                        lsp_types::SymbolKind::STRUCT => "struct",
                        lsp_types::SymbolKind::EVENT => "event",
                        lsp_types::SymbolKind::OPERATOR => "operator",
                        lsp_types::SymbolKind::TYPE_PARAMETER => "type",
                        _ => "symbol",
                    };
                    app.outline_items.push(crate::app::SymbolItem {
                        name: sym.name.clone(),
                        kind,
                        line: sym.location.range.start.line as usize,
                        tab_index: Some(app.active_tab),
                        file_name: None,
                        file_path: None,
                    });
                }
                app.outline_items.sort_by_key(|item| item.line);
                *needs_redraw = true;
            } else {
                app.lsp_outline_available = false;
            }
        }
    }
    // Process workspace symbol responses
    if app.lsp_manager.workspace_symbols_dirty {
        app.lsp_manager.workspace_symbols_dirty = false;
        if app.symbol_list_active && app.symbol_list_workspace {
            app.source_symbol_work.cancel_picker();
            let symbols = std::mem::take(&mut app.lsp_manager.workspace_symbols);
            app.symbol_list_items.clear();
            for sym in &symbols {
                let kind = match sym.kind {
                    lsp_types::SymbolKind::FILE => "file",
                    lsp_types::SymbolKind::MODULE => "module",
                    lsp_types::SymbolKind::NAMESPACE => "namespace",
                    lsp_types::SymbolKind::PACKAGE => "module",
                    lsp_types::SymbolKind::CLASS => "class",
                    lsp_types::SymbolKind::METHOD | lsp_types::SymbolKind::CONSTRUCTOR => {
                        "function"
                    }
                    lsp_types::SymbolKind::PROPERTY => "property",
                    lsp_types::SymbolKind::FIELD => "field",
                    lsp_types::SymbolKind::ENUM => "enum",
                    lsp_types::SymbolKind::INTERFACE => "interface",
                    lsp_types::SymbolKind::FUNCTION => "function",
                    lsp_types::SymbolKind::VARIABLE => "variable",
                    lsp_types::SymbolKind::CONSTANT => "constant",
                    lsp_types::SymbolKind::STRUCT => "struct",
                    lsp_types::SymbolKind::ENUM_MEMBER => "enum",
                    lsp_types::SymbolKind::TYPE_PARAMETER => "type",
                    _ => "symbol",
                };
                let file_name = sym
                    .location
                    .uri
                    .to_file_path()
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
                // Find tab index for this file if it's open
                let file_path = sym.location.uri.to_file_path().ok();
                let tab_index = file_path.as_ref().and_then(|fp| {
                    app.tabs
                        .iter()
                        .position(|t| t.editor.file_path.as_ref() == Some(fp))
                });
                app.symbol_list_items.push(crate::app::SymbolItem {
                    name: sym.name.clone(),
                    kind,
                    line: sym.location.range.start.line as usize,
                    tab_index,
                    file_name,
                    file_path,
                });
            }
            // Re-filter with current query
            app.symbol_list_filtered.clear();
            app.symbol_list_filtered
                .extend(0..app.symbol_list_items.len());
            app.symbol_list_selected = 0;
            *needs_redraw = true;
        }
    }
    // Process folding range responses
    if app.lsp_manager.folding_ranges_dirty {
        app.lsp_manager.folding_ranges_dirty = false;
        let ranges = std::mem::take(&mut app.lsp_manager.folding_ranges);
        app.lsp_folding_ranges = ranges
            .iter()
            .map(|r| (r.start_line as usize, r.end_line as usize))
            .collect();
        app.lsp_folding_ranges.sort_by_key(|&(s, _)| s);
        // Copy to active editor so fold system can use them
        let ranges_clone = app.lsp_folding_ranges.clone();
        if let Some(ed) = app.editor_mut() {
            ed.lsp_folding_ranges = ranges_clone;
        }
        *needs_redraw = true;
    }
    // Process call hierarchy responses
    if app.lsp_manager.call_hierarchy_dirty {
        app.lsp_manager.call_hierarchy_dirty = false;
        let items = std::mem::take(&mut app.lsp_manager.call_hierarchy_items);
        if !items.is_empty() {
            app.call_hierarchy_root_name = items[0].name.clone();
            // Automatically request incoming or outgoing calls
            let item = items[0].clone();
            if app.call_hierarchy_mode == crate::app::CallHierarchyMode::Incoming {
                app.lsp_manager.request_incoming_calls(&item);
            } else {
                app.lsp_manager.request_outgoing_calls(&item);
            }
        } else {
            app.notify("No call hierarchy item found at cursor");
        }
    }
    if app.lsp_manager.call_hierarchy_incoming_dirty {
        app.lsp_manager.call_hierarchy_incoming_dirty = false;
        let calls = std::mem::take(&mut app.lsp_manager.call_hierarchy_incoming);
        app.call_hierarchy_results.clear();
        for call in &calls {
            let path = call.from.uri.to_file_path().unwrap_or_default();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            app.call_hierarchy_results
                .push(crate::app::CallHierarchyEntry {
                    name: call.from.name.clone(),
                    detail: file_name,
                    file_path: path,
                    line: call.from.selection_range.start.line as usize,
                    col: call.from.selection_range.start.character as usize,
                });
        }
        app.call_hierarchy_active = true;
        app.call_hierarchy_selected = 0;
        *needs_redraw = true;
    }
    if app.lsp_manager.call_hierarchy_outgoing_dirty {
        app.lsp_manager.call_hierarchy_outgoing_dirty = false;
        let calls = std::mem::take(&mut app.lsp_manager.call_hierarchy_outgoing);
        app.call_hierarchy_results.clear();
        for call in &calls {
            let path = call.to.uri.to_file_path().unwrap_or_default();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            app.call_hierarchy_results
                .push(crate::app::CallHierarchyEntry {
                    name: call.to.name.clone(),
                    detail: file_name,
                    file_path: path,
                    line: call.to.selection_range.start.line as usize,
                    col: call.to.selection_range.start.character as usize,
                });
        }
        app.call_hierarchy_active = true;
        app.call_hierarchy_selected = 0;
        *needs_redraw = true;
    }
    // Process selection range responses
    if app.lsp_manager.selection_ranges_dirty {
        app.lsp_manager.selection_ranges_dirty = false;
        let ranges = std::mem::take(&mut app.lsp_manager.selection_ranges);
        if let Some(sr) = ranges.into_iter().next().filter(|_| app.lsp_selection_context_current()) {
            // Flatten the linked list: innermost first, outermost last
            let mut flat: Vec<(usize, usize, usize, usize)> = Vec::new();
            let mut current = &sr;
            loop {
                let r = &current.range;
                let valid = app.editor().is_some_and(|ed| {
                    r.start <= r.end && (r.end.line as usize) < ed.line_count()
                });
                if !valid { break; }
                flat.push((
                    r.start.line as usize,
                    app.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, r.start.line as usize, r.start.character)),
                    r.end.line as usize,
                    app.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, r.end.line as usize, r.end.character)),
                ));
                if let Some(ref parent) = current.parent {
                    current = parent;
                } else {
                    break;
                }
            }
            app.lsp_selection_ranges = flat;
        }
        *needs_redraw = true;
    }
    // Process code lens responses
    if app.lsp_manager.code_lenses_dirty {
        app.lsp_manager.code_lenses_dirty = false;
        let lenses = std::mem::take(&mut app.lsp_manager.code_lenses);
        app.lsp_code_lenses.clear();
        app.lsp_code_lenses_raw.clear();
        // Send resolve requests for lenses without commands
        let fp = app.editor().and_then(|ed| ed.file_path.clone());
        for lens in &lenses {
            if lens.command.is_none() && lens.data.is_some() {
                if let Some(ref fp) = fp {
                    app.lsp_manager.request_code_lens_resolve(fp, lens);
                }
            }
            if let Some(ref cmd) = lens.command {
                let line = lens.range.start.line as usize;
                let title = cmd.title.clone();
                if let Some(existing) = app.lsp_code_lenses.iter_mut().find(|(l, _)| *l == line) {
                    existing.1.push_str(" | ");
                    existing.1.push_str(&title);
                } else {
                    app.lsp_code_lenses.push((line, title));
                }
            }
        }
        app.lsp_code_lenses_raw = lenses;
        app.lsp_code_lenses.sort_by_key(|&(line, _)| line);
        *needs_redraw = true;
    }
    // Process resolved code lenses
    if app.lsp_manager.code_lens_resolve_dirty {
        app.lsp_manager.code_lens_resolve_dirty = false;
        let resolved = std::mem::take(&mut app.lsp_manager.code_lens_resolve_results);
        for lens in resolved {
            if let Some(ref cmd) = lens.command {
                let line = lens.range.start.line as usize;
                let title = cmd.title.clone();
                if let Some(existing) = app.lsp_code_lenses.iter_mut().find(|(l, _)| *l == line) {
                    existing.1.push_str(" | ");
                    existing.1.push_str(&title);
                } else {
                    app.lsp_code_lenses.push((line, title));
                    app.lsp_code_lenses.sort_by_key(|&(line, _)| line);
                }
                // Update the raw lens too
                if let Some(raw) = app
                    .lsp_code_lenses_raw
                    .iter_mut()
                    .find(|l| l.range == lens.range && l.command.is_none())
                {
                    raw.command = lens.command.clone();
                }
            }
        }
        *needs_redraw = true;
    }
    // Process type hierarchy responses
    if app.lsp_manager.type_hierarchy_dirty {
        app.lsp_manager.type_hierarchy_dirty = false;
        let items = std::mem::take(&mut app.lsp_manager.type_hierarchy_items);
        if !items.is_empty() {
            app.type_hierarchy_root_name = items[0].name.clone();
            let item = items[0].clone();
            if app.type_hierarchy_mode == crate::app::TypeHierarchyMode::Supertypes {
                app.lsp_manager.request_supertypes(&item);
            } else {
                app.lsp_manager.request_subtypes(&item);
            }
        } else {
            app.notify("No type hierarchy item found at cursor");
        }
    }
    if app.lsp_manager.type_hierarchy_supertypes_dirty {
        app.lsp_manager.type_hierarchy_supertypes_dirty = false;
        let items = std::mem::take(&mut app.lsp_manager.type_hierarchy_supertypes);
        app.type_hierarchy_results.clear();
        for item in &items {
            let path = item.uri.to_file_path().unwrap_or_default();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            app.type_hierarchy_results
                .push(crate::app::CallHierarchyEntry {
                    name: item.name.clone(),
                    detail: file_name,
                    file_path: path,
                    line: item.selection_range.start.line as usize,
                    col: item.selection_range.start.character as usize,
                });
        }
        app.type_hierarchy_active = true;
        app.type_hierarchy_selected = 0;
        *needs_redraw = true;
    }
    if app.lsp_manager.type_hierarchy_subtypes_dirty {
        app.lsp_manager.type_hierarchy_subtypes_dirty = false;
        let items = std::mem::take(&mut app.lsp_manager.type_hierarchy_subtypes);
        app.type_hierarchy_results.clear();
        for item in &items {
            let path = item.uri.to_file_path().unwrap_or_default();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            app.type_hierarchy_results
                .push(crate::app::CallHierarchyEntry {
                    name: item.name.clone(),
                    detail: file_name,
                    file_path: path,
                    line: item.selection_range.start.line as usize,
                    col: item.selection_range.start.character as usize,
                });
        }
        app.type_hierarchy_active = true;
        app.type_hierarchy_selected = 0;
        *needs_redraw = true;
    }
    // Process document link responses
    if app.lsp_manager.document_links_dirty {
        app.lsp_manager.document_links_dirty = false;
        let links = std::mem::take(&mut app.lsp_manager.document_links);
        app.lsp_document_links.clear();
        for link in &links {
            let line = link.range.start.line as usize;
            let start_col = app.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, line, link.range.start.character));
            let end_col = app.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, line, link.range.end.character));
            let target = link
                .target
                .as_ref()
                .map(|u| u.to_string())
                .or_else(|| link.tooltip.clone())
                .unwrap_or_default();
            if !target.is_empty() {
                app.lsp_document_links
                    .push((line, start_col, end_col, target));
            }
        }
        app.lsp_document_links
            .sort_by_key(|&(line, col, _, _)| (line, col));
        *needs_redraw = true;
    }
    // Process on-type formatting responses
    if app.lsp_manager.on_type_format_dirty {
        app.lsp_manager.on_type_format_dirty = false;
        let edits = std::mem::take(&mut app.lsp_manager.on_type_format_edits);
        let target = app.lsp_on_type_format_target.take();
        let response_is_current = target.is_some_and(|target| app.editor().is_some_and(|editor| {
            editor.file_path.as_ref() == Some(&target.path) && editor.content_version == target.content_version
        }));
        if response_is_current && !edits.is_empty() {
            if let Some(editor) = app.editor_mut() {
                let _ = crate::app_lsp_edits::apply(editor, &edits);
            }
            *needs_redraw = true;
        }
    }
    if app.lsp_goto_pending && app.lsp_manager.definition_dirty && app.lsp_manager.definition_locations.is_empty() {
        app.lsp_goto_pending = false;
        app.notify("No definition found");
    }
    if app.lsp_references_pending && app.lsp_manager.references_dirty && app.lsp_manager.reference_locations.is_empty() {
        app.lsp_references_pending = false;
        app.notify("No references found");
    }
    app.lsp_manager.definition_dirty = false;
    app.lsp_manager.references_dirty = false;
    // Handle go-to-definition results
    if app.lsp_goto_pending && !app.lsp_manager.definition_locations.is_empty() {
        app.lsp_goto_pending = false;
        let loc = app.lsp_manager.definition_locations[0].clone();
        if let Ok(path) = loc.uri.to_file_path() {
            let line = loc.range.start.line as usize;
            let col = loc.range.start.character as usize;
            app.push_nav_history();
            app.open_file(&path);
            if let Some(ed) = app.editor_mut() {
                ed.clear_selection();
                ed.set_lsp_cursor(line, col as u32);
                ed.center_cursor();
            }
            app.finish_navigation();
            *needs_redraw = true;
        }
    }

    // Handle LSP references results
    if app.lsp_references_pending && !app.lsp_manager.reference_locations.is_empty() {
        app.lsp_references_pending = false;
        app.lsp_references_active = true;
        app.lsp_references_selected = 0;
        *needs_redraw = true;
    }

    // Handle LSP format document results
    if app.handle_lsp_format_result() {
        *needs_redraw = true;
    }

    // Handle LSP rename results
    // Handle prepareRename response
    if app.lsp_prepare_rename_pending && app.lsp_manager.prepare_rename_result.is_some() {
        app.lsp_prepare_rename_pending = false;
        if let Some((range, placeholder)) = app.lsp_manager.prepare_rename_result.take() {
            // Use placeholder from LSP if available, otherwise word at cursor
            let word = placeholder
                .or_else(|| {
                    // If range is provided, extract text from that range
                    range.and_then(|r| {
                        app.editor().and_then(|ed| {
                            let (start, end) = crate::lsp_positions::range(&ed.rope, r)?;
                            Some(ed.rope.slice(start..end).to_string())
                        })
                    })
                })
                .or_else(|| app.editor().and_then(crate::editor::Editor::word_at_cursor));

            if let Some(word) = word {
                app.rename.symbol_active = true;
                app.rename.symbol_old = word.clone();
                app.rename.symbol_new = word;
                app.rename.symbol_cursor = app.rename.symbol_new.len();
                app.rename.symbol_selection_anchor = Some(0);
            }
            *needs_redraw = true;
        }
    }

    if app.lsp_rename_pending && app.lsp_manager.rename_edit.is_some() {
        app.lsp_rename_pending = false;
        if let Some(edit) = app.lsp_manager.rename_edit.take() {
            let versions = std::mem::take(&mut app.lsp_manager.rename_request_versions);
            app.queue_lsp_workspace_edit_snapshot(edit, "Renamed symbol".into(), versions, None);
            *needs_redraw = true;
        }
    }

    // Apply pre-rename edits from workspace/willRenameFiles
    if let Some(edit) = app.lsp_manager.will_rename_edit.take() {
        app.queue_lsp_workspace_edit(edit, "Applied pre-rename edits".into(), None, None);

    }
    for (process, id, edit) in std::mem::take(&mut app.lsp_manager.workspace_edit_requests) {
        app.queue_lsp_workspace_edit(edit, "Applied language server edit".into(), Some((process, id)), None);
    }
    *needs_redraw |= app.poll_lsp_workspace_edits();

}

#[cfg(test)]
#[path = "app_lsp_scheduling_tests.rs"]
mod scheduling_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Tab};
    use crate::editor::Editor;
    use std::path::PathBuf;

    #[test]
    fn inlay_hint_kind_changes_repaint_even_when_the_label_and_position_are_unchanged() {
        let mut app = make_app_with_file("aaa\nbbb\nccc\nddd\n", "test.rs");
        app.lsp_inlay_hint_last_scroll = 0;
        for (kind, expected_redraw) in [(1, true), (2, true), (2, false)] {
            app.lsp_manager.inlay_hints = vec![serde_json::from_value(serde_json::json!({
                "position":{"line":2,"character":3}, "label":[{"value":"type"},{"value":"hint"}], "kind":kind
            })).unwrap()];
            app.lsp_manager.inlay_hints_dirty = true;
            app.lsp_manager.inlay_hint_response_target =
                Some(crate::lsp::DocumentFeedbackRequest {
                    target: crate::background::LspDocumentTarget {
                        path: PathBuf::from("test.rs"),
                        content_version: app.editor().unwrap().content_version,
                    },
                    range: Some(inlay_range(app.editor().unwrap(), None)),
                    client_idx: 0,
                });
            let mut needs_redraw = false;
            poll_lsp(&mut app, &mut needs_redraw);
            assert_eq!(needs_redraw, expected_redraw);
            assert_eq!(app.lsp_inlay_hints.len(), 1);
            let hint = &app.lsp_inlay_hints[0];
            assert_eq!((hint.line, hint.column, hint.label.as_str()), (2, 3, "typehint"));
            assert_eq!(hint.color_kind_index(), kind);
        }
    }

    fn make_edit(
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        new_text: &str,
    ) -> lsp_types::TextEdit {
        lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position {
                    line: start_line,
                    character: start_char,
                },
                end: lsp_types::Position {
                    line: end_line,
                    character: end_char,
                },
            },
            new_text: new_text.to_string(),
        }
    }

    fn make_app_with_file(text: &str, path: &str) -> App {
        let mut app = App::new(PathBuf::from("."));
        let mut editor = Editor::test_with_text(text);
        editor.file_path = Some(PathBuf::from(path));
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app
    }

    #[test]
    fn apply_single_edit_replaces_text() {
        let mut app = make_app_with_file("hello world\n", "test.rs");
        let edits = vec![make_edit(0, 6, 0, 11, "rust")];
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert_eq!(count, 1);
        assert_eq!(app.editor().unwrap().rope.to_string(), "hello rust\n");
    }

    #[test]
    fn apply_insert_edit() {
        let mut app = make_app_with_file("ab\n", "test.rs");
        let edits = vec![make_edit(0, 1, 0, 1, "XY")];
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert_eq!(count, 1);
        assert_eq!(app.editor().unwrap().rope.to_string(), "aXYb\n");
    }

    #[test]
    fn apply_delete_edit() {
        let mut app = make_app_with_file("abcdef\n", "test.rs");
        let edits = vec![make_edit(0, 2, 0, 4, "")];
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert_eq!(count, 1);
        assert_eq!(app.editor().unwrap().rope.to_string(), "abef\n");
    }

    #[test]
    fn apply_multiple_edits_in_reverse_order() {
        let mut app = make_app_with_file("aaa bbb ccc\n", "test.rs");
        let edits = vec![make_edit(0, 0, 0, 3, "AAA"), make_edit(0, 8, 0, 11, "CCC")];
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert_eq!(count, 2);
        let text = app.editor().unwrap().rope.to_string();
        assert!(text.contains("AAA"));
        assert!(text.contains("CCC"));
    }

    #[test]
    fn apply_multiline_edit() {
        let mut app = make_app_with_file("line1\nline2\nline3\n", "test.rs");
        let edits = vec![make_edit(0, 0, 1, 5, "replaced")];
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert_eq!(count, 1);
        assert_eq!(app.editor().unwrap().rope.to_string(), "replaced\nline3\n");
    }

    #[test]
    fn apply_edit_out_of_bounds_line_is_skipped() {
        let mut app = make_app_with_file("only\n", "test.rs");
        let edits = vec![make_edit(5, 0, 5, 3, "nope")];
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert_eq!(count, 0);
        assert_eq!(app.editor().unwrap().rope.to_string(), "only\n");
    }

    #[test]
    fn apply_edit_marks_editor_modified() {
        let mut app = make_app_with_file("hello\n", "test.rs");
        app.editor_mut().unwrap().modified = false;
        let edits = vec![make_edit(0, 0, 0, 5, "world")];
        apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert!(app.editor().unwrap().modified);
    }

    #[test]
    fn apply_empty_edits_returns_zero() {
        let mut app = make_app_with_file("hello\n", "test.rs");
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), vec![]);
        assert_eq!(count, 0);
    }

    #[test]
    fn apply_edit_clamps_column_to_line_length() {
        let mut app = make_app_with_file("hi\n", "test.rs");
        // Column 100 is way past end of "hi" line
        let edits = vec![make_edit(0, 0, 0, 100, "replaced")];
        let count = apply_lsp_text_edits(&mut app, &PathBuf::from("test.rs"), edits);
        assert_eq!(count, 1);
    }
}
