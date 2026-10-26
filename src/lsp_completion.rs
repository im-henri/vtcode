//! Normalize completion candidates once, instead of allocating in a comparator.
use super::*;

impl LspManager {
    pub(crate) fn cancel_completion_resolve(&mut self) {
        let id = self.active_resolve_id.take();
        self.cancel_request_id(id);
        self.completion_resolve_index = None;
    }

    /// Resolve only the selected row. Successful, unsupported and failed results
    /// are cached for this completion list; cancelled requests can be retried.
    pub(crate) fn request_completion_resolve(&mut self, path: &Path, index: usize) -> bool {
        if self.completion_resolve_index != Some(index) {
            self.cancel_completion_resolve();
        }
        if self.completion_resolved_items.contains(&index)
            || (self.completion_resolve_index == Some(index) && self.active_resolve_id.is_some())
        {
            return true;
        }
        let Some(item) = self.completion_items.get(index) else {
            return false;
        };
        let Some(client) = self.clients.iter_mut().find(|client| {
            client.initialized
                && !client.server_dead
                && client.handles_file(path)
                && client.is_file_open(path)
        }) else {
            return false;
        };
        let Some(original) = item
            .original_item
            .as_ref()
            .filter(|_| client.supports_request("completionItem/resolve"))
        else {
            self.completion_resolved_items.insert(index);
            return true;
        };
        // Borrow the original item; no duplicate item/JSON payload is retained
        // just to search the entire result list when the response arrives.
        match client.request_completion_resolve(original) {
            Ok(id) => {
                self.active_resolve_id = Some(id);
                self.completion_resolve_index = Some(index);
            }
            Err(_) => {
                self.completion_resolved_items.insert(index);
            }
        }
        true
    }

    pub(crate) fn finish_completion_resolve(&mut self, id: i64, item: Option<CompletionItem>) {
        if self.active_resolve_id != Some(id) {
            return;
        }
        self.active_resolve_id = None;
        let Some(index) = self.completion_resolve_index.take() else {
            return;
        };
        self.completion_resolved_items.insert(index);
        let (Some(completion), Some(item)) = (self.completion_items.get_mut(index), item) else {
            return;
        };
        if let Some(original) = completion.original_item.as_mut() {
            if item.documentation.is_some() { original.documentation = item.documentation.clone(); }
            if item.label_details.is_some() { original.label_details = item.label_details.clone(); }
        }
        if let Some(documentation) = item.documentation {
            completion.documentation = Some(match documentation {
                Documentation::String(text) => text,
                Documentation::MarkupContent(content) => content.value,
            });
        }
        if let Some(detail) = item.detail {
            completion.detail = Some(detail);
        }
        if let Some(edits) = item.additional_text_edits {
            completion.additional_text_edits = edits;
        }
        self.completion_resolve_dirty = true;
        self.completion_revision = self.completion_revision.wrapping_add(1);
    }
}

/// Executed by the server reader before it wakes the UI. The event queue owns
/// display-ready rows, so even a large result takes only a vector swap to show.
pub(crate) fn prepare_completions(
    mut items: Vec<CompletionItem>,
    prefix: Option<&str>,
) -> Vec<LspCompletionItem> {
    if let Some(prefix) = prefix {
        let query = prefix.to_lowercase();
        if !query.is_empty() {
            // Servers may return the whole symbol table. Filter once on the
            // reader thread, using filterText and Unicode-aware subsequences.
            items.retain(|item| {
                let mut remaining = query.chars();
                let mut wanted = remaining.next();
                for ch in item.filter_text.as_deref().unwrap_or(&item.label).chars().flat_map(char::to_lowercase) {
                    if wanted == Some(ch) {
                        wanted = remaining.next();
                        if wanted.is_none() { return true; }
                    }
                }
                false
            });
        }
        sort_completions(&mut items, prefix);
    }
    items
        .into_iter()
        .map(|item| {
            // Extract text_edit range and insert text
            let (insert_text, text_edit_range) = match &item.text_edit {
                Some(CompletionTextEdit::Edit(te)) => {
                    let r = &te.range;
                    (
                        te.new_text.clone(),
                        Some((
                            (r.start.line, r.start.character),
                            (r.end.line, r.end.character),
                        )),
                    )
                }
                Some(CompletionTextEdit::InsertAndReplace(ir)) => {
                    // Use the replace range (broader)
                    let r = &ir.replace;
                    (
                        ir.new_text.clone(),
                        Some((
                            (r.start.line, r.start.character),
                            (r.end.line, r.end.character),
                        )),
                    )
                }
                None => (
                    item.insert_text
                        .clone()
                        .unwrap_or_else(|| item.label.clone()),
                    None,
                ),
            };
            let doc = item.documentation.as_ref().map(|d| match d {
                Documentation::String(s) => s.clone(),
                Documentation::MarkupContent(mc) => mc.value.clone(),
            });
            let is_snippet = item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET);
            LspCompletionItem {
                label: item.label.clone(),
                detail: item.detail.clone(),
                insert_text,
                kind: item.kind,
                text_edit_range,
                additional_text_edits: item.additional_text_edits.clone().unwrap_or_default(),
                is_snippet,
                original_item: Some(item),
                documentation: doc,
            }
        })
        .collect()
}

pub(crate) fn sort_completions(items: &mut [CompletionItem], prefix: &str) {
    let prefix = prefix.to_lowercase();
    if items.iter().any(|item| item.sort_text.is_some()) {
        // LSP defines label as the fallback for missing sortText. Use one
        // ordering for the whole list; mixing comparators per pair is not total.
        items.sort_by_cached_key(|item| {
            (
                LspManager::completion_match_rank(
                    &item
                        .filter_text
                        .as_ref()
                        .unwrap_or(&item.label)
                        .to_lowercase(),
                    &prefix,
                ),
                item.sort_text.as_ref().unwrap_or(&item.label).clone(),
                item.label.clone(),
            )
        });
    } else {
        items.sort_by_cached_key(|item| {
            let normalized = item
                .filter_text
                .as_ref()
                .unwrap_or(&item.label)
                .to_lowercase();
            (
                LspManager::completion_match_rank(&normalized, &prefix),
                LspManager::completion_kind_rank(item.kind),
                u8::from(item.label.contains("::") || item.label.contains('.')),
                item.label.len(),
                normalized,
                item.label.clone(),
            )
        });
    }
}

#[cfg(test)]
#[path = "lsp_completion_resolve_tests.rs"]
mod resolve_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_completions_filter_unrelated_rows_but_keep_fuzzy_and_unicode_matches() {
        let items = || vec![
            CompletionItem { label: "Étoile_buffer".into(), sort_text: Some("1".into()), ..Default::default() },
            CompletionItem { label: "different label".into(), filter_text: Some("éB".into()), sort_text: Some("0".into()), ..Default::default() },
            CompletionItem { label: "unrelated".into(), ..Default::default() },
        ];
        assert_eq!(prepare_completions(items(), Some("Éb")).iter().map(|item| item.label.as_str()).collect::<Vec<_>>(),
            ["different label", "Étoile_buffer"]);
        assert_eq!(prepare_completions(items(), Some("")).len(), 3);
        assert!(prepare_completions(items(), Some("missing")).is_empty());
    }

    #[test]
    fn partial_sort_text_uses_label_fallback_and_a_consistent_order() {
        let mut items = vec![
            CompletionItem {
                label: "z".into(),
                sort_text: Some("a".into()),
                ..Default::default()
            },
            CompletionItem {
                label: "c".into(),
                ..Default::default()
            },
            CompletionItem {
                label: "b".into(),
                sort_text: Some("z".into()),
                ..Default::default()
            },
        ];
        for _ in 0..3 {
            items.rotate_left(1);
            sort_completions(&mut items, "");
            assert_eq!(
                items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
                ["z", "c", "b"]
            );
        }
    }

    #[test]
    fn filter_text_and_unicode_match_rank_precede_server_order() {
        let mut items = vec![
            CompletionItem {
                label: "first".into(),
                sort_text: Some("0".into()),
                ..Default::default()
            },
            CompletionItem {
                label: "second".into(),
                filter_text: Some("Éclair".into()),
                sort_text: Some("9".into()),
                ..Default::default()
            },
        ];
        sort_completions(&mut items, "é");
        assert_eq!(items[0].label, "second");
    }

    #[test]
    fn fallback_keeps_local_functions_ahead_of_qualified_or_unmatched_items() {
        let mut items: Vec<_> = ["pkg.foo", "food", "bar", "foo"]
            .into_iter()
            .map(|label| CompletionItem {
                label: label.into(),
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            })
            .collect();
        sort_completions(&mut items, "fo");
        assert_eq!(
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
            ["foo", "food", "pkg.foo", "bar"]
        );
    }
}
