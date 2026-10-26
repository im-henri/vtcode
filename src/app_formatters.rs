//! Formatter selection is independent of language-server configuration.
use crate::{
    app::{App, TabKind},
    formatters::{Config, Provider},
    settings::{Settings, SettingsScope},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct Request {
    pub path: PathBuf,
    pub version: u64,
    pub document: u64,
    pub selection: bool,
    pub lines: Option<(usize, usize)>,
    pub save_after: bool,
}

#[cfg(test)]
#[path = "app_formatters_tests.rs"]
mod tests;

pub(crate) struct External {
    request: Request,
    cancelled: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<crate::formatters::Prepared, String>>,
}
impl Drop for External {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

impl App {
    pub(crate) fn formatting_request(
        &self,
        selection: bool,
        save_after: bool,
    ) -> Result<Request, String> {
        let tab = self
            .tabs
            .get(self.active_tab)
            .ok_or("Open a file before formatting.")?;
        let editor = &tab.editor;
        if tab.kind != TabKind::File || self.diff_selection_owns_cursor() {
            return Err(
                "This view cannot be formatted. Open the working file in a text editor.".into(),
            );
        }
        if editor.bg_loading {
            return Err("The file is still loading. Try again when it has opened.".into());
        }
        if editor.read_only {
            return Err("The file is read-only. Make it writable before formatting.".into());
        }
        if editor.large_file_mode {
            return Err("Formatting is unavailable in large-file mode.".into());
        }
        let path = editor
            .file_path
            .clone()
            .ok_or("Save this file first so a formatter can identify it.")?;
        let lines = if selection {
            let (start, _, end, col) = editor
                .selection_range()
                .ok_or("Select some text before using Format Selection.")?;
            Some((
                start + 1,
                if end > start && col == 0 {
                    end
                } else {
                    end + 1
                },
            ))
        } else {
            None
        };
        Ok(Request {
            path,
            version: editor.content_version,
            document: editor.navigation_id,
            selection,
            lines,
            save_after,
        })
    }

    pub(crate) fn is_formatting(&self) -> bool {
        self.lsp_format_pending || self.formatting.external.is_some()
    }

    pub(crate) fn uses_lsp_formatter(&self) -> bool {
        self.editor()
            .and_then(|editor| self.formatters.get(&crate::formatters::language(editor)))
            .is_some_and(|config| config.provider == Provider::Lsp)
    }

    pub(crate) fn request_format(&mut self, selection: bool, save_after: bool) -> bool {
        let request = match self.formatting_request(selection, save_after) {
            Ok(request) => request,
            Err(error) => {
                self.show_formatting_error(error);
                return false;
            }
        };
        if self.is_formatting() {
            if save_after && self.lsp_format_pending && self.editor().is_some_and(|editor| self.formatting.has_document_request(&request.path, request.version, editor.save_identity)) {
                self.lsp_save_after_format_path = Some(request.path);
                return true;
            }
            if save_after
                && let Some(external) = &mut self.formatting.external
                && external.request.path == request.path
                && external.request.version == request.version
                && external.request.document == request.document
            {
                external.request.save_after = true;
                return true;
            }
            self.show_formatting_error(
                "A formatting request is already running. Wait for it to finish, then try again.",
            );
            return !save_after;
        }
        let language = crate::formatters::language(self.editor().unwrap());
        let Some(config) = self.formatters.get(&language).cloned() else {
            self.open_formatter_settings(Some(request));
            return true;
        };
        if let Err(error) = crate::formatters::validate(&language, &config) {
            self.show_formatting_error(error);
            return false;
        }
        match config.provider {
            Provider::Lsp => self.request_lsp_format(selection, save_after),
            Provider::Disabled => {
                if !save_after {
                    self.notify("Formatting is disabled for this language. Change it in Formatter settings.");
                }
                false
            }
            Provider::Command => {
                let rope = self.editor().unwrap().rope.clone();
                let root = if self.remote_workspace.is_some() && !crate::remote::workspace::is_view_key(&request.path) {
                    request.path.parent().unwrap_or(std::path::Path::new(".")).to_path_buf()
                } else { self.explorer.root.clone() };
                let cancelled = Arc::new(AtomicBool::new(false));
                let worker_cancelled = Arc::clone(&cancelled);
                let target = request.clone();
                let (sender, receiver) = mpsc::channel();
                let wake = self.background.wake_handle();
                let remote = self.remote_workspace.clone().filter(|_| crate::remote::workspace::is_view_key(&request.path));
                self.background.spawn("formatter", move || {
                    let result = if let Some(remote) = remote {
                        (|| -> Result<_, String> {
                            let cancel = crate::background::CancellationToken::from_flag(worker_cancelled);
                            let output = remote.format_document(&target.path, config, &rope, target.lines, &cancel).map_err(|error| error.to_string())?;
                            Ok(crate::formatters::prepare(&rope, &output))
                        })()
                    } else { crate::formatters::run(
                        &config,
                        &target.path,
                        &root,
                        &rope,
                        target.lines,
                        worker_cancelled,
                        Duration::from_secs(30),
                    ) };
                    let _ = sender.send(result);
                    wake.notify();
                });
                self.formatting.error = None;
                self.formatting.feedback = None;
                self.formatting.external = Some(External {
                    request,
                    cancelled,
                    receiver,
                });
                true
            }
        }
    }

    pub(crate) fn poll_external_format(&mut self) -> bool {
        let Some(pending) = self.formatting.external.as_ref() else {
            return false;
        };
        let request = &pending.request;
        let target = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .find(|tab| tab.editor.navigation_id == request.document);
        let invalid = match target.map(|tab| &tab.editor) {
            None => Some("The document was closed before formatting finished."),
            Some(editor) if editor.file_path.as_ref() != Some(&request.path) =>
                Some("The document was renamed while formatting. Run Format Document again."),
            Some(editor) if editor.read_only =>
                Some("The document became read-only before formatting finished."),
            Some(editor) if editor.bg_loading =>
                Some("The document is being reloaded. Run Format Document when it has finished."),
            Some(editor) if editor.content_version != request.version =>
                Some("The document changed while formatting. Your newer edits were kept; run Format Document again."),
            _ => None,
        };
        let result = if let Some(reason) = invalid {
            Err(reason.into())
        } else {
            match pending.receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(_) => Err("The formatter stopped without returning a result.".into()),
            }
        };
        let request = request.clone();
        self.formatting.external = None;
        let result = result.and_then(|prepared| {
            let tab = self
                .tabs
                .iter_mut()
                .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
                .find(|tab| tab.editor.navigation_id == request.document)
                .ok_or("The formatted document was closed.")?;
            prepared.apply(&mut tab.editor)
        });
        if result.is_ok() { self.sync_document_view(request.document); }
        self.report_formatting_result(result, request.save_after.then_some(request.path));
        true
    }

    /// Patch only the chosen language. Preserve unrelated and unknown settings,
    /// and do not apply a choice that could not be persisted.
    pub(crate) fn save_formatter(
        &mut self,
        scope: SettingsScope,
        language: &str,
        config: Option<Config>,
    ) -> Result<(), String> {
        if let Some(config) = &config {
            crate::formatters::validate(language, config)?;
        }
        self.patch_settings_scope(scope, "formatters", |settings: &mut Settings| {
            let map = settings.formatters.get_or_insert_with(Default::default);
            if let Some(config) = &config {
                map.insert(language.into(), config.clone());
            } else {
                map.remove(language);
            }
        }).map_err(|error| format!("Could not save formatter: {error}"))
    }
}
