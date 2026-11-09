//! Snapshot the clicked document on the UI thread; read and diff off-thread.
use std::path::PathBuf;

use ratatui::layout::Rect;

use crate::{
    app::App,
    background::{BackgroundResult, CancellationToken},
    remote::{Command, Reply},
    ui_hunk_peek::{HunkPeek, PeekTarget, UnsavedHunkSnapshot},
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
    pub result: Result<Option<HunkPeek>, String>,
}

pub(crate) struct Continuation {
    generation: u64,
    next: usize,
    // A Git action label while applying; None while loading its successor.
    action: Option<&'static str>,
}

struct Request {
    remote: Option<crate::remote::workspace::Workspace>,
    root: PathBuf,
    file: Option<PathBuf>,
    repository: Option<PathBuf>,
    target: PeekTarget,
    status: char,
    document: Option<(u64, u64)>,
    tab_size: usize,
    live: Option<ropey::Rope>,
    saved: Option<ropey::Rope>,
}

impl Request {
    fn load(self, cancel: &CancellationToken) -> Result<Option<HunkPeek>, String> {
        let check_cancelled = || {
            if cancel.is_cancelled() { Err("Preview closed".to_string()) } else { Ok(()) }
        };
        check_cancelled()?;
        if self.status == 'N' {
            let (document, version) = self.document.ok_or("The document is no longer open")?;
            let saved = self.saved.ok_or("Saved version is unavailable")?;
            let current = self.live.ok_or("The document is no longer open")?;
            let saved_text = saved.to_string();
            let current_text = current.to_string();
            let prepared = match self.target {
                PeekTarget::Line(line) => HunkPeek::from_texts(&saved_text, &current_text, line, self.tab_size).map(Some),
                target => HunkPeek::from_texts_at(&saved_text, &current_text, target, self.tab_size),
            }?;
            let Some(mut peek) = prepared else { return Ok(None); };
            check_cancelled()?;
            peek.unsaved = Some(UnsavedHunkSnapshot { document, version, saved, current });
            return Ok(Some(peek));
        }
        let mut prepared = self.load_git(self.status == 'S', cancel)?;
        if prepared.is_none() && matches!(self.target, PeekTarget::Line(_)) {
            // A marker can outlive staging/unstaging until the background
            // refresh arrives. Check the other source once, at this exact
            // line. Continuations must end when their source has no hunks.
            check_cancelled()?;
            prepared = self.load_git(self.status != 'S', cancel)?;
        }
        let Some(mut peek) = prepared else { return Ok(None); };
        check_cancelled()?;
        if self.remote.is_none() { peek.remote = None; }
        Ok(Some(peek))
    }

    fn load_git(&self, staged: bool, cancel: &CancellationToken) -> Result<Option<HunkPeek>, String> {
        let file = self.file.as_ref().ok_or("No file selected")?;
        let sources = if let Some(remote) = &self.remote {
            let file = remote.remote_path(file).map_err(|error| error.to_string())?;
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
        if cancel.is_cancelled() { return Err("Preview closed".into()); }
        crate::remote::hunk::prepare_target(sources, staged, self.live.as_ref(), self.target, self.tab_size)
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
            file, repository,
            target: self.hunk_preview_continuation.as_ref().filter(|next| next.action.is_none())
                .map_or(PeekTarget::Line(self.hunk_popup_line), |next| PeekTarget::Hunk(next.next)),
            status, document, tab_size, live, saved,
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
        if current != result.document && !cancelled
            && current.map(|document| document.0) == result.document.map(|document| document.0)
            && let Some(next) = self.hunk_preview_continuation.as_ref().filter(|next| next.action.is_none())
        {
            // A discard can finish reloading the clean editor while the next
            // preview is in flight. Read its new snapshot instead of closing.
            let index = next.next;
            self.advance_hunk_preview(index);
            return true;
        }
        if current != result.document || cancelled {
            self.close_hunk_popup();
            return true;
        }
        match result.result {
            Ok(Some(peek)) => {
                let advance = self.hunk_preview_continuation.take().is_some();
                let changed_source = self.hunk_popup_status != 'N' && (self.hunk_popup_status == 'S') != peek.staged;
                if self.hunk_popup_status != 'N' { self.hunk_popup_status = if peek.staged { 'S' } else { 'U' }; }
                self.hunk_preview_feedback = None;
                self.hunk_peek = Some(peek);
                if advance { crate::ui_hunk_peek::reveal_current_change(self); }
                if changed_source { self.refresh_git_line_status_async(); }
            }
            Ok(None) if self.hunk_popup_status != 'N' || self.hunk_preview_continuation.is_some() => {
                let git = self.hunk_popup_status != 'N';
                self.close_hunk_popup();
                if git { self.refresh_git_line_status_async(); }
            }
            result => {
                let error = result.err().unwrap_or_else(|| "No changes found".into());
                self.hunk_preview_feedback = Some(Feedback::new(FeedbackState::Failed(error)));
                self.hunk_peek = None;
            }
        }
        true
    }

    pub(crate) fn retry_hunk_preview(&mut self) {
        if let Some(next) = &self.hunk_preview_continuation {
            let index = next.next;
            self.advance_hunk_preview(index);
            return;
        }
        let (x, y) = self.hunk_popup_pos;
        self.open_hunk_popup(x, y, self.hunk_popup_file.clone(), self.hunk_popup_line, self.hunk_popup_status);
    }

    #[cfg(test)]
    pub(crate) fn load_hunk_peek(&mut self) -> Result<HunkPeek, String> {
        self.hunk_preview_request().load(&CancellationToken::new())?
            .ok_or_else(|| "No changes found".into())
    }

    pub(crate) fn hunk_preview_action_pending(&self) -> bool {
        self.hunk_preview_continuation.as_ref().is_some_and(|next| next.action.is_some())
    }

    pub(crate) fn begin_hunk_preview_action(&mut self, action: &'static str) {
        self.hunk_preview_continuation = self.hunk_peek.as_ref().map(|peek| Continuation {
            generation: self.hunk_popup_generation, next: peek.selected, action: Some(action),
        });
    }

    pub(crate) fn finish_hunk_preview_action(&mut self, action: &str, success: bool) {
        let Some(next) = self.hunk_preview_continuation.as_ref().filter(|next| {
            next.generation == self.hunk_popup_generation && next.action == Some(action)
        }) else { return; };
        let index = next.next;
        self.hunk_preview_continuation = None;
        if success && self.hunk_popup_active { self.advance_hunk_preview(index); }
    }

    pub(crate) fn advance_hunk_preview(&mut self, next: usize) {
        if !self.hunk_popup_active { return; }
        if let Some(feedback) = self.hunk_preview_feedback.take() { feedback.cancel(); }
        self.hunk_popup_generation = self.hunk_popup_generation.wrapping_add(1);
        self.hunk_peek = None;
        self.hunk_preview_continuation = Some(Continuation {
            generation: self.hunk_popup_generation, next, action: None,
        });
        self.request_hunk_preview();
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
