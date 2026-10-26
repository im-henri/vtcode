//! Large paste transactions: keep input/rendering local and responsive, retain
//! one pending payload, and publish a prepared Rope atomically after validation.
use crate::{
    app::{ActivePane, App, NotifyLevel},
    background::{BackgroundResult, CancellationToken},
};
use crossterm::event::{PasteFailure, PasteStatus};

pub(crate) const BACKGROUND_BYTES: usize = 256 * 1024;
const NOTICE: &str = "bulk-paste";
const ACTIONS: &[crate::ui_side_notice::Action] = &[crate::ui_side_notice::Action {
    label: "Cancel",
    command: "cancel_paste",
}];

#[derive(PartialEq, Eq)]
struct Focus {
    pane: ActivePane,
    navigation: Option<u64>,
    document: Option<u64>,
    input: Option<crate::text_input::TextInputId>,
    terminal: Option<usize>,
}

impl Focus {
    fn capture(app: &App) -> Self {
        let input = app.focused_text_input_id();
        let terminal = app.focused_terminal_index();
        Self {
            pane: app.active_pane,
            navigation: app.editor().map(|editor| editor.navigation_id),
            document: (app.active_pane == ActivePane::Editor
                && input.is_none()
                && terminal.is_none())
            .then(|| app.editor().map(|editor| editor.save_identity))
            .flatten(),
            input,
            terminal,
        }
    }
}

struct Job {
    serial: u64,
    navigation: u64,
    document: u64,
    cancel: CancellationToken,
}

#[derive(Default)]
pub(crate) struct State {
    serial: u64,
    job: Option<Job>,
    receiving: Option<Focus>,
    receiving_cancelled: bool,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.cancel();
        }
    }
}

fn size(bytes: usize) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
#[path = "large_paste_tests.rs"]
mod tests;

impl App {
    pub(crate) fn notify_copy_failure(&mut self) {
        let reason = crate::clipboard::last_write_error()
            .unwrap_or_else(|| "System clipboard unavailable".into());
        self.notify_warn(format!(
            "{reason}; copied inside VTCode only. Use Edit > Paste to use the internal copy."
        ));
        self.side_notice.show(
            "copy-error",
            crate::ui_side_notice::Notice {
                title: "Copied inside VTCode only",
                message: reason,
                level: NotifyLevel::Warning,
                actions: &[],
            },
        );
    }
    fn paste_notice(&mut self, title: &'static str, message: String) {
        self.side_notice.publish(
            NOTICE,
            crate::ui_side_notice::Notice {
                title,
                message,
                level: NotifyLevel::Info,
                actions: ACTIONS,
            },
        );
    }

    pub(crate) fn paste_error(&mut self, message: String) {
        self.notify_error(message.clone());
        self.side_notice.show(
            "paste-error",
            crate::ui_side_notice::Notice {
                title: "Paste could not be completed",
                message,
                level: NotifyLevel::Error,
                actions: &[],
            },
        );
    }

    pub(crate) fn terminal_paste_status(&mut self, status: PasteStatus) {
        match status {
            PasteStatus::Receiving { bytes } => {
                if self.bulk_paste.job.is_some() {
                    // The decoder may already have queued the completed event.
                    // Retain the rejection even if the original worker finishes
                    // before that event reaches the UI.
                    if self.bulk_paste.receiving.is_none() {
                        self.bulk_paste.receiving = Some(Focus::capture(self));
                    }
                    let already_cancelled =
                        std::mem::replace(&mut self.bulk_paste.receiving_cancelled, true);
                    crossterm::event::cancel_paste();
                    if !already_cancelled {
                        self.notify_warn(
                            "A paste is already being prepared; the additional paste was cancelled",
                        );
                    }
                    return;
                }
                if self.bulk_paste.receiving.is_none() {
                    self.bulk_paste.receiving = Some(Focus::capture(self));
                    self.bulk_paste.receiving_cancelled = false;
                    self.side_notice.remove(NOTICE);
                }
                if self.bulk_paste.receiving_cancelled {
                    crossterm::event::cancel_paste();
                    return;
                }
                self.paste_notice(
                    "Receiving pasted text…",
                    format!("{} received · document unchanged", size(bytes)),
                );
            }
            PasteStatus::Failed { reason, .. } => {
                self.bulk_paste.receiving = None;
                self.bulk_paste.receiving_cancelled = false;
                let reason = match reason {
                    PasteFailure::Cancelled => "Paste cancelled; no pasted text was inserted",
                    PasteFailure::Interrupted => {
                        "The terminal interrupted the paste; no pasted text was inserted"
                    }
                    PasteFailure::AllocationFailed => {
                        "Not enough memory to receive the paste; no pasted text was inserted"
                    }
                };
                if self.bulk_paste.job.is_some() {
                    self.notify_warn(reason);
                    return;
                }
                self.side_notice.remove(NOTICE);
                self.paste_error(reason.into());
            }
        }
    }

    pub(crate) fn accept_terminal_paste(&mut self) -> bool {
        let Some(focus) = self.bulk_paste.receiving.take() else {
            return true;
        };
        self.side_notice.remove(NOTICE);
        if std::mem::take(&mut self.bulk_paste.receiving_cancelled) {
            self.notify("Paste cancelled; the document was not changed");
            return false;
        }
        if focus != Focus::capture(self) {
            self.paste_error(
                "Paste cancelled because its destination changed while receiving text".into(),
            );
            return false;
        }
        true
    }

    pub(crate) fn cancel_pending_paste(&mut self) {
        if let Some(job) = &self.bulk_paste.job {
            job.cancel.cancel();
            self.paste_notice(
                "Cancelling paste…",
                "The document has not been changed".into(),
            );
        } else if self.bulk_paste.receiving.is_some() {
            self.bulk_paste.receiving_cancelled = true;
            crossterm::event::cancel_paste();
            self.paste_notice(
                "Cancelling paste…",
                "Discarding the remaining terminal input".into(),
            );
        }
    }

    pub(crate) fn paste_pending_for(&self, navigation: u64) -> bool {
        self.bulk_paste
            .job
            .as_ref()
            .is_some_and(|job| job.navigation == navigation)
    }

    pub(crate) fn pending_paste_document(&self) -> Option<u64> {
        self.bulk_paste
            .job
            .as_ref()
            .map(|job| job.document)
            .or_else(|| {
                self.bulk_paste
                    .receiving
                    .as_ref()
                    .and_then(|focus| focus.document)
            })
    }

    pub(crate) fn paste_in_progress(&self) -> bool {
        self.bulk_paste.job.is_some() || self.bulk_paste.receiving.is_some()
    }

    pub(crate) fn paste_editor_clipboard(&mut self, smart: bool) {
        if self.bulk_paste.job.is_some() {
            self.notify_warn("A paste is already being prepared; wait or cancel it first");
            return;
        }
        if self
            .editor()
            .is_none_or(|editor| editor.read_only || editor.bg_loading)
        {
            self.notify_warn("The target document is read-only or still loading");
            return;
        }
        let text = match crate::clipboard::get_text() {
            Ok(text) => {
                if let Some(editor) = self.editor_mut() {
                    editor.clipboard_failed = false;
                }
                text
            }
            Err(arboard::Error::ContentNotAvailable)
                if self.editor().is_none_or(|editor| !editor.clipboard_failed) =>
            {
                self.notify("Clipboard contains no text");
                return;
            }
            Err(error) => {
                let fallback = self
                    .editor()
                    .map(|editor| editor.clipboard.clone())
                    .filter(|text| !text.is_empty());
                match fallback {
                    Some(text) => {
                        self.notify_warn(format!(
                            "System clipboard unavailable ({error}); using the internal copy"
                        ));
                        text
                    }
                    None => {
                        self.paste_error(format!("Cannot read clipboard: {error}"));
                        return;
                    }
                }
            }
        };
        self.paste_editor_text(text, smart);
    }

    pub(crate) fn paste_editor_text(&mut self, text: String, smart: bool) {
        if self.bulk_paste.job.is_some() {
            self.notify_warn("A paste is already being prepared; wait or cancel it first");
            return;
        }
        if text.is_empty() {
            self.notify("Clipboard contains no text");
            return;
        }
        let Some(editor) = self.editor_mut() else {
            return;
        };
        if editor.read_only || editor.bg_loading {
            self.paste_error("The target document is read-only or still loading".into());
            return;
        }
        if text.len() < BACKGROUND_BYTES || editor.markdown_preview_scroll.is_some() {
            if smart {
                editor.paste_with_text(&text);
            } else {
                editor.paste_snapshot_text(&text, false, None);
            }
            editor.clipboard = text;
            return;
        }
        let Some(snapshot) = crate::editor_paste::Snapshot::capture(editor, &text) else {
            return;
        };
        let document = editor.save_identity;
        let navigation = snapshot.navigation;
        let bytes = text.len();
        self.bulk_paste.serial = self.bulk_paste.serial.wrapping_add(1);
        let serial = self.bulk_paste.serial;
        let cancel = CancellationToken::new();
        let work_cancel = cancel.clone();
        let activity = self
            .background
            .activity
            .start("Preparing paste", None)
            .cancellable(cancel.clone());
        let sender = self.background.sender.clone();
        let submitted = self.background.interactive_sender().send(Box::new(move || {
            let _activity = activity.running();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                snapshot.prepare(text, smart, &work_cancel)
            }))
            .unwrap_or_else(|_| {
                Err("Paste preparation failed; the document was not changed".into())
            });
            let _ = sender.send(BackgroundResult::PastePrepared {
                serial,
                result: result.map(Box::new),
            });
        }));
        if submitted.is_err() {
            self.paste_error("Paste worker is unavailable; the document was not changed".into());
            return;
        }
        self.bulk_paste.job = Some(Job {
            serial,
            navigation,
            document,
            cancel,
        });
        self.side_notice.remove(NOTICE);
        self.paste_notice(
            "Preparing paste…",
            format!("{} · document unchanged until ready", size(bytes)),
        );
    }

    pub(crate) fn finish_paste(
        &mut self,
        serial: u64,
        result: Result<Box<crate::editor_paste::Prepared>, String>,
    ) {
        if self
            .bulk_paste
            .job
            .as_ref()
            .is_none_or(|job| job.serial != serial)
        {
            return;
        }
        let job = self.bulk_paste.job.take().unwrap();
        self.side_notice.remove(NOTICE);
        if job.cancel.is_cancelled() {
            self.notify("Paste cancelled; the document was not changed");
            return;
        }
        let result = result.and_then(|prepared| {
            let threshold = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
            let editor = self
                .tabs
                .iter_mut()
                .chain(
                    self.groups
                        .iter_mut()
                        .flat_map(|group| group.tabs.iter_mut()),
                )
                .find(|tab| tab.editor.navigation_id == job.navigation)
                .map(|tab| &mut tab.editor)
                .ok_or_else(|| {
                    "Paste cancelled because the destination was closed or moved".to_owned()
                })?;
            prepared.apply(editor, threshold)
        });
        match result {
            Ok(bytes) => {
                self.last_edit_time = std::time::Instant::now();
                self.notify(format!("Paste complete · {}", size(bytes)));
                if self
                    .editor()
                    .is_some_and(|editor| editor.navigation_id == job.navigation)
                {
                    self.update_search();
                }
            }
            Err(error) => self.paste_error(error),
        }
    }
}
