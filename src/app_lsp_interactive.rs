//! Interactive requests follow the exact asynchronous document snapshot.
use crate::app::App;
use std::path::PathBuf;

pub(crate) struct PendingSuggestion {
    path: PathBuf,
    version: u64,
    cursor: (usize, usize),
    prefix: String,
    sent: bool,
    selection: Option<(String, String, Option<String>)>,
}

#[derive(Default)]
pub(crate) struct LspSuggestions {
    completion: Option<PendingSuggestion>,
    signature: Option<PendingSuggestion>,
}

impl App {
    pub(crate) fn request_lsp_suggestions(&mut self, signature: bool) -> bool {
        let Some(editor) = self
            .editor()
            .filter(|editor| !editor.large_file_mode && !editor.bg_loading && !editor.read_only)
        else {
            return false;
        };
        let Some(path) = editor
            .file_path
            .clone()
            .filter(|path| self.lsp_manager.has_config_for_file(path))
        else {
            return false;
        };
        let rope = editor.rope.clone();
        let version = editor.content_version;
        let prefix = editor.word_prefix_at_cursor().unwrap_or_default();
        let continuing = !signature
            && self.lsp_completion_active
            && self
                .lsp_suggestions
                .completion
                .as_ref()
                .is_some_and(|previous| {
                    previous.path == path
                        && previous.cursor.0 == editor.cursor_line
                        && previous
                            .cursor
                            .1
                            .saturating_sub(previous.prefix.chars().count())
                            == editor.cursor_col.saturating_sub(prefix.chars().count())
                });
        let selection = continuing
            .then(|| {
                self.lsp_manager
                    .completion_items
                    .get(self.lsp_completion_selected)
            })
            .flatten()
            .map(|item| {
                (
                    item.label.clone(),
                    item.insert_text.clone(),
                    item.original_item
                        .as_ref()
                        .map_or_else(|| item.detail.clone(), |original| original.detail.clone()),
                )
            })
            .or_else(|| {
                continuing
                    .then(|| self.lsp_suggestions.completion.as_ref()?.selection.clone())
                    .flatten()
            });
        let request = PendingSuggestion {
            path: path.clone(),
            version,
            cursor: (editor.cursor_line, editor.cursor_col),
            prefix,
            sent: false,
            selection,
        };
        if signature {
            self.lsp_signature_active = true;
            self.lsp_manager.cancel_signature_request();
            self.lsp_manager.signature_help = None;
            self.lsp_suggestions.signature = Some(request);
        } else {
            self.completion_ui.begin_request(continuing);
            self.lsp_completion_active = true;
            self.lsp_manager.cancel_completion_request();
            self.lsp_completion_selected = 0;
            self.lsp_manager.completion_items.clear();
            self.lsp_suggestions.completion = Some(request);
        }
        if self.lsp_manager.last_notified_version.get(&path) != Some(&version) {
            self.queue_lsp_document_sync(path, version, rope);
        }
        self.dispatch_lsp_suggestions();
        if signature {
            self.lsp_signature_active
        } else {
            self.lsp_completion_active
        }
    }

    pub(crate) fn finish_lsp_completion_response(&mut self) {
        let items = &self.lsp_manager.completion_items;
        let previous = self
            .lsp_suggestions
            .completion
            .as_ref()
            .and_then(|request| request.selection.as_ref());
        self.lsp_completion_selected = previous
            .and_then(|(label, insert_text, detail)| {
                items.iter().position(|item| {
                    item.label == *label
                        && item.insert_text == *insert_text
                        && item
                            .original_item
                            .as_ref()
                            .map_or(item.detail.as_ref(), |original| original.detail.as_ref())
                            == detail.as_ref()
                })
            })
            .or_else(|| {
                items.iter().position(|item| {
                    item.original_item
                        .as_ref()
                        .is_some_and(|item| item.preselect == Some(true))
                })
            })
            .unwrap_or(0);
        self.completion_ui.status = items.is_empty().then_some("No suggestions");
    }

    pub(crate) fn completion_snapshot_current(&self) -> bool {
        self.lsp_suggestions
            .completion
            .as_ref()
            .is_none_or(|pending| {
                self.editor().is_some_and(|editor| {
                    editor.file_path.as_ref() == Some(&pending.path)
                        && editor.content_version == pending.version
                        && (editor.cursor_line, editor.cursor_col) == pending.cursor
                        && !editor.bg_loading
                })
            })
    }

    pub(crate) fn dispatch_lsp_suggestions(&mut self) {
        for signature in [false, true] {
            let pending = if signature {
                self.lsp_suggestions.signature.take()
            } else {
                self.lsp_suggestions.completion.take()
            };
            let Some(mut pending) = pending else {
                continue;
            };
            let active = if signature {
                self.lsp_signature_active
            } else {
                self.lsp_completion_active
            };
            let current = active
                && self.editor().is_some_and(|editor| {
                    editor.file_path.as_ref() == Some(&pending.path)
                        && editor.content_version == pending.version
                        && (editor.cursor_line, editor.cursor_col) == pending.cursor
                        && !editor.bg_loading
                });
            let failed = self.lsp_manager.configs.iter().any(|config| {
                config.handles_file(&pending.path)
                    && self
                        .lsp_manager
                        .startup_errors
                        .contains_key(&config.language)
            });
            if !current || failed {
                if signature {
                    self.lsp_signature_active = false;
                    self.lsp_manager.cancel_signature_request();
                } else {
                    self.lsp_completion_active = false;
                    self.lsp_manager.cancel_completion_request();
                    self.lsp_manager.completion_items.clear();
                }
                continue;
            }
            let server_ready = self
                .lsp_manager
                .clients
                .iter()
                .any(|client| client.initialized && client.handles_file(&pending.path));
            if !pending.sent
                && server_ready
                && self.lsp_manager.last_notified_version.get(&pending.path)
                    == Some(&pending.version)
            {
                let column = crate::lsp_positions::to_utf16(
                    &self.editor().unwrap().rope,
                    pending.cursor.0,
                    pending.cursor.1,
                );
                let accepted = if signature {
                    self.lsp_manager.request_signature_help(
                        &pending.path,
                        pending.cursor.0 as u32,
                        column,
                    )
                } else {
                    self.lsp_manager.request_completion(
                        &pending.path,
                        pending.cursor.0 as u32,
                        column,
                        &pending.prefix,
                    )
                };
                if !accepted {
                    if signature {
                        self.lsp_signature_active = false;
                    } else {
                        self.lsp_completion_active = false;
                    }
                    continue;
                }
                pending.sent = true;
            }
            if signature {
                self.lsp_suggestions.signature = Some(pending);
            } else {
                self.lsp_suggestions.completion = Some(pending);
            }
        }
    }
}
