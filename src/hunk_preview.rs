//! Snapshot the clicked document on the UI thread; read and diff off-thread.
use std::path::PathBuf;

use ratatui::layout::Rect;

use crate::{
    app::App,
    background::{BackgroundResult, CancellationToken},
    remote::{Command, Reply},
    ui_hunk_peek::{HunkPeek, UnsavedHunkSnapshot},
};

pub(crate) enum FeedbackState {
    Loading(CancellationToken),
    Failed(String),
}

pub(crate) struct Feedback {
    pub state: FeedbackState,
    pub area: Option<Rect>,
    pub focused_button: Option<usize>,
    pub hovered_button: Option<usize>,
}

impl Feedback {
    fn new(state: FeedbackState) -> Self {
        Self { state, area: None, focused_button: None, hovered_button: None }
    }

    pub(crate) fn loading(&self) -> bool {
        matches!(self.state, FeedbackState::Loading(_))
    }

    pub(crate) fn cancel(&self) {
        if let FeedbackState::Loading(cancel) = &self.state {
            cancel.cancel();
        }
    }
}

pub(crate) struct PeekResult {
    pub generation: u64,
    pub document: Option<(u64, u64)>,
    pub result: Result<HunkPeek, String>,
}

struct Request {
    remote: Option<crate::remote::workspace::Workspace>,
    root: PathBuf,
    file: Option<PathBuf>,
    repository: Option<PathBuf>,
    line: usize,
    status: char,
    document: Option<(u64, u64)>,
    tab_size: usize,
    live: Option<ropey::Rope>,
    saved: Option<ropey::Rope>,
}

impl Request {
    fn load(self, cancel: &CancellationToken) -> Result<HunkPeek, String> {
        let check_cancelled = || {
            if cancel.is_cancelled() { Err("Preview closed".to_string()) } else { Ok(()) }
        };
        check_cancelled()?;
        if self.status == 'N' {
            let (document, version) = self.document.ok_or("The document is no longer open")?;
            let saved = self.saved.ok_or("Saved version is unavailable")?;
            let current = self.live.ok_or("The document is no longer open")?;
            let mut peek = HunkPeek::from_texts(
                &saved.to_string(), &current.to_string(), self.line, self.tab_size,
            )?;
            check_cancelled()?;
            peek.unsaved = Some(UnsavedHunkSnapshot { document, version, saved, current });
            return Ok(peek);
        }
        let file = self.file.ok_or("No file selected")?;
        let staged = self.status == 'S';
        let sources = if let Some(remote) = &self.remote {
            let file = remote.remote_path(&file).map_err(|error| error.to_string())?;
            let repository = self.repository.as_ref()
                .map(|path| remote.remote_path(path)).transpose().map_err(|error| error.to_string())?;
            let reply = remote.client.call_cancellable(
                Command::HunkSources { file, repository, staged }, cancel,
            ).map_err(|error| error.to_string())?;
            let Reply::HunkSources(sources) = reply else {
                return Err("Invalid hunk preview reply".into());
            };
            sources
        } else {
            // Reuse the same exact Git sources and line mapping as headless.
            // All path resolution, filesystem reads and Git work happen here.
            let native = (|| -> std::io::Result<_> {
                let root = std::path::absolute(&self.root)?;
                let file = crate::remote::git::from_native(&std::path::absolute(file)?, &root)?;
                let repository = self.repository.as_ref().map(|path| {
                    crate::remote::git::from_native(&std::path::absolute(path)?, &root)
                }).transpose()?;
                crate::remote::hunk::native_sources(&root, file, repository, staged, self.live.as_ref())
            })();
            native.map_err(|error| error.to_string())?
        };
        check_cancelled()?;
        let mut peek = crate::remote::hunk::prepare(
            sources, staged, self.live.as_ref(), self.line, self.tab_size,
        )?;
        check_cancelled()?;
        if self.remote.is_none() {
            peek.remote = None;
        }
        Ok(peek)
    }
}

impl App {
    fn hunk_preview_editor(&mut self) -> Option<&mut crate::editor::Editor> {
        if self.hunk_popup_status == 'N' {
            let path = self.hunk_popup_file.clone();
            self.editor_mut().filter(|editor| editor.file_path == path)
        } else {
            let path = self.hunk_popup_file.clone()?;
            self.find_editor_mut_by_path(&path)
        }
    }

    fn hunk_preview_request(&mut self) -> Request {
        let file = self.hunk_popup_file.clone();
        let repository = file.as_deref().and_then(|path| self.repository_hint_for_path(path));
        let status = self.hunk_popup_status;
        let editor = self.hunk_preview_editor();
        let document = editor.as_ref().map(|editor| (editor.navigation_id, editor.content_version));
        let tab_size = editor.as_ref().map_or(4, |editor| editor.tab_size);
        let live = editor.as_ref().filter(|editor| editor.modified || status == 'N')
            .map(|editor| editor.rope.clone());
        let saved = editor.and_then(|editor| editor.saved_rope.clone());
        Request {
            remote: self.remote_workspace.clone(), root: self.explorer.root.clone(),
            file, repository, line: self.hunk_popup_line, status, document, tab_size, live, saved,
        }
    }

    pub(crate) fn request_hunk_preview(&mut self) {
        let request = self.hunk_preview_request();
        let document = request.document;
        let generation = self.hunk_popup_generation;
        // Unsaved-buffer previews also work when Source Control is disabled.
        // The preview generation/document identity owns these results.
        let sender = self.background.sender.clone();
        let cancel = self.background.spawn_cancellable("hunk-preview", move |cancel| {
            let result = request.load(&cancel);
            let _ = sender.send(BackgroundResult::HunkPeek(PeekResult { generation, document, result }));
        });
        self.hunk_preview_feedback = Some(Feedback::new(FeedbackState::Loading(cancel)));
    }

    pub(crate) fn apply_hunk_preview(&mut self, result: PeekResult) -> bool {
        if !self.hunk_popup_active || self.hunk_popup_generation != result.generation {
            return false;
        }
        let current = self.hunk_preview_editor().map(|editor| (editor.navigation_id, editor.content_version));
        let cancelled = self.hunk_preview_feedback.as_ref().is_some_and(|feedback| {
            matches!(&feedback.state, FeedbackState::Loading(cancel) if cancel.is_cancelled())
        });
        if current != result.document || cancelled {
            self.close_hunk_popup();
            return true;
        }
        match result.result {
            Ok(peek) => {
                self.hunk_preview_feedback = None;
                self.hunk_peek = Some(peek);
            }
            Err(error) => {
                self.hunk_preview_feedback = Some(Feedback::new(FeedbackState::Failed(error)));
                self.hunk_peek = None;
            }
        }
        true
    }

    pub(crate) fn retry_hunk_preview(&mut self) {
        let (x, y) = self.hunk_popup_pos;
        self.open_hunk_popup(x, y, self.hunk_popup_file.clone(), self.hunk_popup_line, self.hunk_popup_status);
    }

    #[cfg(test)]
    pub(crate) fn load_hunk_peek(&mut self) -> Result<HunkPeek, String> {
        self.hunk_preview_request().load(&CancellationToken::new())
    }

    #[cfg(test)]
    #[track_caller]
    pub(crate) fn wait_for_hunk_preview(&mut self) {
        self.wait_for_background(std::time::Duration::from_secs(10), |app| {
            !app.hunk_preview_feedback.as_ref().is_some_and(Feedback::loading)
        });
    }
}

#[cfg(test)]
#[path = "hunk_preview_tests.rs"]
mod tests;
