//! Explicit formatting follows the current buffer, including server startup.
use crate::app::{App, TabKind};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const FEEDBACK_LIFETIME: Duration = Duration::from_secs(4);

struct PendingFormat {
    path: PathBuf,
    document: u64,
    version: u64,
    range: Option<lsp_types::Range>,
    tab_size: u32,
    insert_spaces: bool,
    deadline: Instant,
    sent: bool,
}

#[cfg(test)]
#[path = "app_lsp_format_tests.rs"]
mod tests;

#[derive(Default)]
pub(crate) struct FormattingState {
    pending: Option<PendingFormat>,
    pub(crate) external: Option<crate::app_formatters::External>,
    pub(crate) error: Option<String>,
    pub(crate) feedback: Option<(String, Instant)>,
}

impl FormattingState {
    pub(crate) fn has_document_request(&self, path: &std::path::Path, version: u64, document: u64) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.path == path && pending.version == version && pending.document == document && pending.range.is_none())
    }
    pub(crate) fn next_wait(&self) -> Option<Duration> {
        let now = Instant::now();
        self.pending
            .as_ref()
            .map(|pending| pending.deadline)
            .into_iter()
            .chain(self.feedback.as_ref().map(|(_, until)| *until))
            .min()
            .map(|until| until.saturating_duration_since(now))
    }
}

impl App {
    pub(crate) fn show_formatting_error(&mut self, message: impl Into<String>) {
        // Server messages are untrusted display text, not terminal controls.
        let message: String = message
            .into()
            .chars()
            .filter(|ch| !ch.is_control() || *ch == '\n')
            .take(1000)
            .collect();
        self.notify_error(format!("Formatting failed: {message}"));
        self.status_message = "Formatting failed".into();
        self.formatting.feedback = None;
        self.formatting.error = Some(message);
    }

    /// Returns true when this request owns completion (and an optional save).
    pub(crate) fn request_lsp_format(&mut self, selection: bool, save_after: bool) -> bool {
        if self.lsp_format_pending {
            self.show_formatting_error(
                "A formatting request is already running. Wait for it to finish, then try again.",
            );
            return !save_after;
        }
        let invalid = match self.tabs.get(self.active_tab) {
            None => Some("Open a file before formatting."),
            Some(tab) if tab.kind != TabKind::File || self.diff_selection_owns_cursor() => {
                Some("This view cannot be formatted. Open the working file in a text editor.")
            }
            Some(tab) if tab.editor.bg_loading => {
                Some("The file is still loading. Try again when it has opened.")
            }
            Some(tab) if tab.editor.read_only => {
                Some("The file is read-only. Make it writable before formatting.")
            }
            Some(tab) if tab.editor.large_file_mode => {
                Some("Formatting is unavailable in large-file mode.")
            }
            Some(tab) if tab.editor.file_path.is_none() => {
                Some("Save this file first so a language server can format it.")
            }
            _ if !self.lsp_enabled => {
                Some("Language servers are disabled. Enable them in Language Servers settings.")
            }
            _ => None,
        };
        if let Some(message) = invalid {
            self.show_formatting_error(message);
            return false;
        }
        let editor = self.editor().unwrap();
        let range = if selection {
            let Some((sl, sc, el, ec)) = editor.selection_range() else {
                self.show_formatting_error("Select some text before using Format Selection.");
                return false;
            };
            Some(lsp_types::Range::new(
                lsp_types::Position::new(
                    sl as u32,
                    crate::lsp_positions::to_utf16(&editor.rope, sl, sc),
                ),
                lsp_types::Position::new(
                    el as u32,
                    crate::lsp_positions::to_utf16(&editor.rope, el, ec),
                ),
            ))
        } else {
            None
        };
        let path = editor.file_path.clone().unwrap();
        let version = editor.content_version;
        let rope = editor.rope.clone();
        self.formatting.pending = Some(PendingFormat {
            path: path.clone(),
            document: editor.save_identity,
            version,
            range,
            tab_size: editor.tab_size as u32,
            insert_spaces: !editor.use_tabs,
            deadline: Instant::now() + REQUEST_TIMEOUT,
            sent: false,
        });
        self.formatting.error = None;
        self.formatting.feedback = None;
        self.lsp_format_pending = true;
        self.lsp_format_target_path = Some(path.clone());
        self.lsp_save_after_format_path = save_after.then(|| path.clone());
        self.lsp_manager.format_error = None;
        self.lsp_manager.format_dirty = false;
        self.queue_lsp_document_sync(path, version, rope);
        self.flush_lsp_document_syncs();
        self.poll_lsp_format_request();
        true
    }

    pub(crate) fn poll_lsp_format_request(&mut self) -> bool {
        let expired = self
            .formatting
            .feedback
            .as_ref()
            .is_some_and(|(_, until)| *until <= Instant::now());
        if expired {
            self.formatting.feedback = None;
        }
        let Some(pending) = self.formatting.pending.as_ref() else {
            return expired;
        };
        if self.lsp_manager.format_dirty {
            return expired;
        }
        let path = pending.path.clone();
        let version = pending.version;
        let document = pending.document;
        if self
            .find_editor_mut_by_path(&path)
            .is_none_or(|editor| editor.content_version != version || editor.save_identity != document)
        {
            self.finish_lsp_formatting(Err(
                "The document changed or was closed while formatting. Run Format Document again."
                    .into(),
            ));
            return true;
        }
        let pending = self.formatting.pending.as_ref().unwrap();
        if Instant::now() >= pending.deadline {
            self.finish_lsp_formatting(Err("The language server did not finish formatting within 30 seconds. Try again or restart it in Language Servers settings.".into()));
            return true;
        }
        if let Some(error) = self
            .lsp_manager
            .configs
            .iter()
            .filter(|config| config.handles_file(&path))
            .find_map(|config| self.lsp_manager.startup_errors.get(&config.language))
        {
            self.finish_lsp_formatting(Err(format!("Could not run the language server: {error}")));
            return true;
        }
        let Some(client) = self
            .lsp_manager
            .clients
            .iter()
            .find(|client| client.handles_file(&path))
        else {
            self.finish_lsp_formatting(Err("No language server is configured for this file. Configure one with formatting support in Language Servers settings.".into()));
            return true;
        };
        if pending.sent || !client.initialized {
            return expired;
        }
        let method = if pending.range.is_some() {
            "textDocument/rangeFormatting"
        } else {
            "textDocument/formatting"
        };
        if !client.supports_request(method) {
            let command = self
                .lsp_manager
                .configs
                .iter()
                .find(|config| config.handles_file(&path))
                .map(|config| config.command.as_str())
                .unwrap_or(&client.language);
            let feature = if pending.range.is_some() {
                "selection formatting"
            } else {
                "document formatting"
            };
            let mut reason = format!(
                "The {} language server ({command}) does not provide {feature}.",
                client.language
            );
            if command.to_ascii_lowercase().contains("pyright") {
                reason.push_str(
                    " Pyright provides Python analysis and completion, but no formatter.",
                );
            }
            reason.push_str(" Choose a formatter that supports this operation in Formatter settings.");
            self.finish_lsp_formatting(Err(reason));
            return true;
        }
        if self.lsp_manager.last_notified_version.get(&path) != Some(&version) {
            self.finish_lsp_formatting(Err("Could not synchronize the current document with its language server. Try restarting the server.".into()));
            return true;
        }
        let accepted = if let Some(range) = pending.range {
            self.lsp_manager
                .request_format_range(&crate::lsp::FormatRangeParams {
                    path: &path,
                    start_line: range.start.line,
                    start_col: range.start.character,
                    end_line: range.end.line,
                    end_col: range.end.character,
                    tab_size: pending.tab_size,
                    insert_spaces: pending.insert_spaces,
                })
        } else {
            self.lsp_manager
                .request_format_document(&path, pending.tab_size, pending.insert_spaces)
        };
        if accepted {
            self.formatting.pending.as_mut().unwrap().sent = true;
        } else {
            let reason = self.lsp_manager.format_error.take().unwrap_or_else(|| {
                "The language server could not accept the formatting request.".into()
            });
            self.finish_lsp_formatting(Err(reason));
        }
        true
    }

    pub(crate) fn finish_lsp_formatting(&mut self, result: Result<usize, String>) {
        let save = self.lsp_save_after_format_path.take();
        self.lsp_format_pending = false;
        self.lsp_format_target_path = None;
        self.formatting.pending = None;
        let id = self.lsp_manager.active_format_id.take();
        self.lsp_manager.cancel_request_id(id);
        self.lsp_manager.format_request_target = None;
        self.lsp_manager.format_dirty = false;
        self.lsp_manager.format_error = None;
        self.lsp_manager.format_edits.clear();
        self.report_formatting_result(result, save);
    }

    pub(crate) fn report_formatting_result(&mut self, result: Result<usize, String>, save: Option<PathBuf>) {
        if let Some(path) = save.as_deref() {
            if let Err(error) = self.save_open_file_by_path(path) {
                self.show_formatting_error(format!("Could not save {}: {error}", path.display()));
                return;
            }
        }
        match result {
            Err(reason) => self.show_formatting_error(if save.is_some() {
                format!("{reason}\nThe file will be saved without formatting.")
            } else {
                reason
            }),
            Ok(count) => {
                let message = if count == 0 {
                    "No formatting changes"
                } else {
                    "Document formatted"
                };
                self.notify(message);
                self.formatting.feedback =
                    Some((message.into(), Instant::now() + FEEDBACK_LIFETIME));
            }
        }
    }

    pub(crate) fn handle_lsp_format_result(&mut self) -> bool {
        if !self.lsp_format_pending || !self.lsp_manager.format_dirty {
            return false;
        }
        let edits = std::mem::take(&mut self.lsp_manager.format_edits);
        let result = if let Some(error) = self.lsp_manager.format_error.take() {
            Err(error)
        } else if let Some(path) = self.lsp_format_target_path.clone() {
            let expected = self
                .lsp_manager
                .format_request_target
                .as_ref()
                .map(|target| target.content_version);
            let document = self.formatting.pending.as_ref().map(|pending| pending.document);
            match self.find_editor_mut_by_path(&path) {
                None => Err("The document was closed before formatting finished.".into()),
                Some(editor)
                    if expected.is_some_and(|version| editor.content_version != version)
                        || document.is_some_and(|document| editor.save_identity != document) =>
                {
                    Err("The document changed while formatting. Run Format Document again.".into())
                }
                Some(editor) => crate::app_lsp_edits::apply(editor, &edits),
            }
        } else {
            Err("The formatting request has no target document.".into())
        };
        self.finish_lsp_formatting(result);
        true
    }
}
