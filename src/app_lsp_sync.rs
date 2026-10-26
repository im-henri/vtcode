//! Coalesce shared editor snapshots before dispatching ordered LSP work.
use super::App;
use std::path::PathBuf;

impl App {
    pub(crate) fn queue_lsp_document_sync(
        &mut self,
        path: PathBuf,
        version: u64,
        rope: ropey::Rope,
    ) {
        if !self.lsp_manager.has_config_for_file(&path)
            || self
                .lsp_manager
                .last_notified_version
                .get(&path)
                .is_some_and(|last| *last >= version)
            || self
                .pending_lsp_syncs
                .get(&path)
                .is_some_and(|(pending, _)| *pending >= version)
        {
            return;
        }
        // No full-buffer allocation or background pool job per keystroke.
        self.pending_lsp_syncs.insert(path, (version, rope));
        self.background.wake_handle().notify();
    }

    pub(crate) fn flush_lsp_document_syncs(&mut self) -> bool {
        if self.pending_lsp_syncs.is_empty() {
            return false;
        }
        let connection_state = (
            self.lsp_manager.clients.len(),
            self.lsp_manager.startup_errors.len(),
        );
        let mut pending = std::mem::take(&mut self.pending_lsp_syncs);
        for (path, (version, rope)) in pending.drain() {
            let current = self
                .tabs
                .iter()
                .chain(self.groups.iter().flat_map(|group| &group.tabs))
                .any(|tab| {
                    tab.kind == super::TabKind::File
                        && tab.editor.file_path.as_ref() == Some(&path)
                        && tab.editor.content_version == version
                        && !tab.editor.bg_loading
                        && !tab.editor.large_file_mode
                });
            if !current {
                continue;
            }
            self.lsp_manager.sync_document(&path, version, rope);
            if self.lsp_manager.last_notified_version.get(&path) != Some(&version) {
                continue;
            }

            // Do not consume another file's pending formatting while flushing
            // a save/open in a different editor group.
            if self
                .pending_lsp_on_type_format
                .as_ref()
                .is_some_and(|request| request.0 == path)
                && let Some((path, line, col, trigger, tab_size, insert_spaces, expected)) =
                    self.pending_lsp_on_type_format.take()
                && expected == version
                && self.lsp_manager.request_on_type_formatting(
                    &crate::lsp::OnTypeFormattingParams {
                        path: &path,
                        line,
                        col,
                        ch: &trigger,
                        tab_size,
                        insert_spaces,
                    },
                )
            {
                self.lsp_on_type_format_target = Some(crate::background::LspDocumentTarget {
                    path,
                    content_version: version,
                });
            }
            if self.pending_lsp_did_save.remove(&path) {
                self.lsp_manager.notify_did_save(&path);
            }
        }
        self.pending_lsp_syncs = pending;
        connection_state
            != (
                self.lsp_manager.clients.len(),
                self.lsp_manager.startup_errors.len(),
            )
    }
}
