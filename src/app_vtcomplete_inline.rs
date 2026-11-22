//! Editor integration for GPTComplete inline predictions (ghost text).
//!
//! Typing asks the shared completion worker for a continuation of the caret
//! line. The answer is painted after the caret without touching the buffer;
//! Tab inserts it as one undoable edit.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::app::{ActivePane, App, TabKind};
use crate::background::{
    BackgroundResult, BackgroundResultSender, BackgroundTaskSender, CancellationToken,
};
use crate::vtcomplete_inline::{Caret, Engine, MAX_DOCUMENT_BYTES, Options, Syntax};

const MAX_BELOW_BYTES: usize = 256 * 1024;
/// Longest the word popup waits for a pending prediction.
const INLINE_HOLD: std::time::Duration = std::time::Duration::from_millis(250);
/// How long the caret must rest before the language model reads ahead there.
/// Reading ahead runs the model on several threads; repeated navigation keys
/// (Ctrl+Down held to scroll) otherwise start and abandon a reading per step.
/// Each move restarts the wait, so a burst of keys reads nothing until it ends.
///
/// Placing the caret precisely (a click, Left/Right, Home/End, word jumps)
/// usually comes right before typing, so it waits briefly.
const INLINE_WARM_PRECISE: std::time::Duration = std::time::Duration::from_millis(150);
/// Moving by lines or pages, or to either end of the file, is mostly reading.
const INLINE_WARM_BROWSING: std::time::Duration = std::time::Duration::from_millis(800);
/// Browsing with no edit for a while is reading a file, not writing it.
const INLINE_WARM_READING: std::time::Duration = std::time::Duration::from_secs(2);
/// Edits within this long make browsing likely part of writing.
const INLINE_RECENT_EDIT: std::time::Duration = std::time::Duration::from_secs(60);

/// The rest required after a navigation key before reading ahead.
fn warm_delay(key: crossterm::event::KeyEvent) -> std::time::Duration {
    use crossterm::event::{KeyCode, KeyModifiers};
    match key.code {
        KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => INLINE_WARM_BROWSING,
        KeyCode::Home | KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            INLINE_WARM_BROWSING
        }
        _ => INLINE_WARM_PRECISE,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Enabled,
    Snoozed,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    group: usize,
    tab: usize,
    document: u64,
    version: u64,
    cursor: (usize, usize),
}

pub(crate) struct Ghost {
    target: Target,
    rope: ropey::Rope,
    pub(crate) text: String,
    /// Accepting also respells the typed start of the word, from this
    /// column of the caret line: `layoutr` becomes `LayoutR`.
    retype: Option<(usize, String)>,
}

struct Job {
    target: Target,
    rope: ropey::Rope,
    syntax: Syntax,
    language: u8,
    /// Asked for with the shortcut rather than by typing.
    explicit: bool,
    /// Nothing is to be shown: the caret came to rest here, and the language
    /// model reads the surroundings ahead of the first request.
    warm: bool,
    /// How confident a prediction must be, from the user's eagerness setting.
    options: Options,
    generation: u64,
    #[cfg(test)]
    with_language_model: bool,
    cancel: CancellationToken,
    sender: BackgroundResultSender,
    activity: crate::completion_activity::Activity,
}

#[derive(Default)]
struct Queue {
    pending: Option<Job>,
    running: bool,
    engine: Option<Box<Engine<'static>>>,
    /// How the answer of this generation spells the typed start of its word.
    retype: Option<(u64, String)>,
    /// The prediction made ahead for the text after accepting the latest one.
    speculation: Option<Speculation>,
}

/// A prediction made ahead for a document that does not exist yet: the
/// current one with the shown prediction accepted.
struct Speculation {
    document: u64,
    cursor: (usize, usize),
    rope: ropey::Rope,
    text: String,
}

/// The document with `text` inserted at `cursor`, and the caret after it.
fn accepted(
    rope: &ropey::Rope,
    cursor: (usize, usize),
    text: &str,
) -> Option<(ropey::Rope, (usize, usize))> {
    let (line, column) = cursor;
    if line >= rope.len_lines() {
        return None;
    }
    let at = rope.line_to_char(line) + column;
    if at > rope.len_chars() {
        return None;
    }
    let mut rope = rope.clone();
    rope.insert(at, text);
    let cursor = match text.rfind('\n') {
        Some(end) => (
            line + text.matches('\n').count(),
            text[end + 1..].chars().count(),
        ),
        None => (line, column + text.chars().count()),
    };
    Some((rope, cursor))
}

#[derive(Default)]
pub(crate) struct State {
    queue: Arc<Mutex<Queue>>,
    snooze_until: Option<Instant>,
    ghost: Option<Ghost>,
    generation: u64,
    cancel: Option<CancellationToken>,
    /// Document and content version when the current key began.
    key_start: Option<(u64, u64)>,
    /// A caret position whose prediction the user dismissed.
    suppressed: Option<Target>,
    /// The caret position of the latest request.
    requested: Option<Target>,
    /// The caret position the language model last read ahead for.
    warmed: Option<Target>,
    /// A caret position to read ahead for once it has rested until then.
    warm_due: Option<(Target, Instant)>,
    /// When the pending prediction was asked for, while the word popup is
    /// held back for it: the prediction is the main suggestion, and a popup
    /// that would flash up only to be covered by it is not shown.
    holding: Option<std::time::Instant>,
    /// Whether the word popup was on screen in the latest frame.
    pub(crate) popup_drawn: bool,
    #[cfg(test)]
    pub(crate) submissions: usize,
    /// Tests run without the language model unless they ask for it here: it
    /// is slow in unoptimized builds.
    #[cfg(test)]
    pub(crate) with_language_model: bool,
}

impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }
}

/// Split a snapshot into the text before the caret, the rest of the caret
/// line and the text below it, each bounded for large documents.
fn split(rope: &ropey::Rope, cursor: (usize, usize)) -> Option<(String, String, String, u64)> {
    let (line, column) = cursor;
    if line >= rope.len_lines() {
        return None;
    }
    let source = rope.line(line);
    let mut line_chars = source.len_chars();
    while line_chars > 0 && matches!(source.char(line_chars - 1), '\n' | '\r') {
        line_chars -= 1;
    }
    let line_start = rope.line_to_char(line);
    let line_end = line_start + line_chars;
    let caret = line_start + column.min(line_chars);
    let caret_byte = rope.char_to_byte(caret);
    if rope.char_to_byte(line_end) - caret_byte > MAX_DOCUMENT_BYTES {
        return None;
    }
    // A window over a very large document starts at a line boundary.
    let limit = MAX_DOCUMENT_BYTES - 4096;
    let first = if caret_byte > limit {
        let line = rope.byte_to_line(caret_byte - limit) + 1;
        rope.line_to_char(line.min(rope.len_lines()))
    } else {
        0
    };
    if first > caret || caret_byte - rope.char_to_byte(first) > limit {
        return None; // One enormous line.
    }
    let before = rope.slice(first..caret).to_string();
    let rest = rope.slice(caret..line_end).to_string();
    let below_start = rope.line_to_char((line + 1).min(rope.len_lines()));
    let below_end_byte = (rope.char_to_byte(below_start) + MAX_BELOW_BYTES).min(rope.len_bytes());
    let below_end = rope.line_to_char(rope.byte_to_line(below_end_byte));
    let below = if below_end > below_start {
        rope.slice(below_start..below_end).to_string()
    } else if below_end_byte == rope.len_bytes() {
        rope.slice(below_start..).to_string()
    } else {
        String::new()
    };
    Some((before, rest, below, first as u64))
}

/// None when cancelled; otherwise the text to show, if any, with the
/// respelling of the typed start of its first word.
fn predict(engine: &mut Engine<'_>, job: &Job) -> Option<Option<(String, Option<String>)>> {
    let cancelled = || job.cancel.is_cancelled();
    if cancelled() {
        return None;
    }
    let Some((before, rest, below, window)) = split(&job.rope, job.target.cursor) else {
        return (!job.warm).then_some(None);
    };
    if cancelled() {
        return None;
    }
    #[cfg(test)]
    if job.with_language_model {
        engine.set_network(crate::vtcomplete_inline::model::embedded());
    }
    let key = job.target.document ^ window.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let caret = Caret {
        key,
        before: &before,
        rest: &rest,
        below: &below,
        syntax: job.syntax,
        language: job.language,
    };
    if job.warm {
        crate::vtcomplete_inline::model::alone(|| engine.warm(caret, &cancelled));
        return None;
    }
    let prediction = engine.predict(
        caret,
        if job.explicit {
            Options::requested(job.options)
        } else {
            job.options
        },
        &cancelled,
    );
    if cancelled() {
        return None;
    }
    Some(prediction.and_then(|prediction| {
        // Completing the word being typed counts as a suggestion too, so
        // the ghost text appears while typing instead of leaving it all to
        // the popup; a single character is not worth showing.
        let useful = job.explicit || prediction.text.chars().count() >= 2;
        useful.then_some((prediction.text, prediction.retype))
    }))
}

fn run(queue: Arc<Mutex<Queue>>) {
    // Helper threads for long computations yield like this worker does.
    crate::vtcomplete_inline::model::set_thread_setup(|| {
        Box::new(crate::worker_priority::CompletionPriority::new())
    });
    let _priority = crate::worker_priority::CompletionPriority::new();
    let mut engine = queue
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .engine
        .take();
    loop {
        let job = {
            let mut queue = queue.lock().unwrap_or_else(|error| error.into_inner());
            match queue.pending.take() {
                Some(job) => job,
                None => {
                    queue.engine = engine;
                    queue.running = false;
                    return;
                }
            }
        };
        if !job.activity.wait(|| job.cancel.is_cancelled()) {
            continue;
        }
        let engine = engine.get_or_insert_with(|| Box::new(Engine::new()));
        if let Some(answer) = predict(engine, &job)
            && !job.cancel.is_cancelled()
        {
            let (text, retype) = answer.unzip();
            let respelled = retype.as_ref().is_some_and(Option::is_some);
            {
                let mut queue = queue.lock().unwrap_or_else(|error| error.into_inner());
                if job.cancel.is_cancelled() {
                    continue;
                }
                queue.retype = retype.flatten().map(|retype| (job.generation, retype));
            }
            let _ = job.sender.send(BackgroundResult::VtCompleteInline {
                generation: job.generation,
                target: job.target.clone(),
                rope: job.rope.clone(),
                text: text.clone(),
            });
            // A reading the answer could not wait for is finished now, for
            // the next request.
            let mut job = job;
            job.warm = true;
            let _ = predict(engine, &job);
            // Then the prediction that follows accepting this one, so that
            // Tab after Tab never waits.
            if let Some(shown) = text.filter(|_| !respelled && !job.cancel.is_cancelled())
                && let Some((rope, cursor)) = accepted(&job.rope, job.target.cursor, &shown)
            {
                job.warm = false;
                job.explicit = false;
                job.rope = rope;
                job.target.cursor = cursor;
                if let Some(Some((next, None))) = predict(engine, &job) {
                    let mut queue = queue.lock().unwrap_or_else(|error| error.into_inner());
                    if !job.cancel.is_cancelled() {
                        queue.speculation = Some(Speculation {
                            document: job.target.document,
                            cursor,
                            rope: job.rope.clone(),
                            text: next,
                        });
                    }
                }
            }
        }
    }
}

impl State {
    fn submit(&mut self, job: Job, pool: BackgroundTaskSender) {
        let (replaced, start) = {
            let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
            let replaced = queue.pending.replace(job);
            let start = !queue.running;
            queue.running = true;
            (replaced, start)
        };
        drop(replaced);
        if start {
            let queue = Arc::clone(&self.queue);
            if pool.send(Box::new(move || run(queue))).is_err() {
                let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
                queue.running = false;
                queue.pending = None;
            }
        }
    }

    fn cancel(&mut self) {
        self.holding = None;
        self.generation = self.generation.wrapping_add(1);
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
    }

    #[cfg(test)]
    pub(crate) fn set_ghost_for_test(&mut self, target: Target, rope: ropey::Rope, text: &str) {
        self.ghost = Some(Ghost {
            target,
            rope,
            text: text.into(),
            retype: None,
        });
    }
}

/// Keys after which a prediction is wanted: ordinary typing, a new line and
/// deleting backwards. Navigation, undo and commands stay quiet.
pub(crate) fn trigger_key(key: crossterm::event::KeyEvent) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    match key.code {
        KeyCode::Char(_) => !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER),
        KeyCode::Enter | KeyCode::Backspace => key.modifiers.is_empty(),
        _ => false,
    }
}

impl App {
    pub(crate) fn inline_availability(&self) -> Availability {
        if !self.completion_preferences.inline_predictions {
            Availability::Disabled
        } else if self.completion_preferences.inline_snoozed
            || self
                .inline_snooze_deadline()
                .is_some_and(|deadline| deadline > Instant::now())
        {
            Availability::Snoozed
        } else {
            Availability::Enabled
        }
    }

    pub(crate) fn inline_snooze_deadline(&self) -> Option<Instant> {
        self.vtcomplete_inline.snooze_until
    }

    pub(crate) fn expire_inline_snooze(&mut self, now: Instant) -> bool {
        self.vtcomplete_inline
            .snooze_until
            .take_if(|deadline| *deadline <= now)
            .is_some()
    }

    pub(crate) fn snooze_inline_predictions(&mut self, duration: Duration) {
        if self.inline_availability() == Availability::Enabled {
            self.vtcomplete_inline.snooze_until = Some(Instant::now() + duration);
            self.stop_inline_work();
        }
    }

    pub(crate) fn reset_inline_prediction_state(&mut self) {
        self.vtcomplete_inline.snooze_until = None;
        self.stop_inline_work();
    }

    pub(crate) fn stop_inline_work(&mut self) {
        self.clear_inline_context();
        // A cancelled worker can finish on the old queue without returning
        // its engine to this editor.
        self.vtcomplete_inline.queue = Default::default();
    }

    pub(crate) fn clear_inline_context(&mut self) {
        self.clear_inline();
        self.vtcomplete_inline.suppressed = None;
        self.vtcomplete_inline.requested = None;
        self.vtcomplete_inline.warmed = None;
        self.vtcomplete_inline.warm_due = None;
        let pending = {
            let mut queue = self
                .vtcomplete_inline
                .queue
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            queue.retype = None;
            queue.speculation = None;
            queue.pending.take()
        };
        drop(pending);
    }

    /// The caret position a prediction would apply to, when the editor is in
    /// a state where ghost text may be shown and accepted.
    pub(crate) fn inline_target(&self) -> Option<Target> {
        if self.inline_availability() != Availability::Enabled
            || self.active_pane != ActivePane::Editor
            || !self.terminal_has_focus
            || self.docked_terminal_has_focus()
            || self.focused_text_input_id().is_some()
            || crate::ui_modal::active(self).is_some()
            || crate::ui_quick_picker::active(self).is_some()
            || self.context_menu_active
            || self.explorer_context_active
            || self.sc_context_active
            || self.shortcuts_active
            || self.settings_active
            || self.tab_context_active
            || self.menu_active
            || self.menu_open
            || self.breadcrumb_dropdown_active
            || self.change_dir_active
            || self.hunk_popup_active
            || self.peek_definition.is_some()
            || self.lsp_code_action_active
            || self.call_hierarchy_active
            || self.type_hierarchy_active
            || self.lsp_references_active
            || self.clipboard_cycle_active
            || self.chord_pending
            || self.paste_in_progress()
            || self.should_render_diff_for_active_editor()
            || self
                .tabs
                .get(self.active_tab)
                .is_none_or(|tab| tab.kind != TabKind::File)
        {
            return None;
        }
        let editor = self.editor()?;
        if editor.read_only
            || editor.bg_loading
            || editor.large_file_mode
            || editor.selection_anchor.is_some()
            || !editor.extra_cursors.is_empty()
            || !editor.snippet_tab_stops.is_empty()
            || editor.markdown_preview_scroll.is_some()
            || editor.cursor_line >= editor.rope.len_lines()
        {
            return None;
        }
        Some(Target {
            group: self.active_group,
            tab: self.active_tab,
            document: editor.navigation_id,
            version: editor.content_version,
            cursor: (editor.cursor_line, editor.cursor_col),
        })
    }

    pub(crate) fn begin_inline_key(&mut self) {
        self.vtcomplete_inline.key_start = self
            .editor()
            .map(|editor| (editor.navigation_id, editor.content_version));
    }

    /// Called after a key was handled. An edit made by typing requests a new
    /// prediction; text typed along a visible prediction keeps its remainder.
    pub(crate) fn finish_inline_key(&mut self, key: crossterm::event::KeyEvent, trigger: bool) {
        let start = self.vtcomplete_inline.key_start.take();
        let Some(target) = self.inline_target() else {
            self.clear_inline();
            return;
        };
        // Accepting a prediction already arranged what follows it.
        if self.vtcomplete_inline.requested.as_ref() == Some(&target)
            || self
                .vtcomplete_inline
                .ghost
                .as_ref()
                .is_some_and(|ghost| ghost.target == target)
        {
            return;
        }
        if !self.quick_suggestions {
            self.clear_inline();
            return;
        }
        let edited = start.is_some_and(|(document, version)| {
            document == target.document && version != target.version
        });
        if !edited {
            if self
                .vtcomplete_inline
                .ghost
                .as_ref()
                .is_some_and(|ghost| ghost.target != target)
            {
                self.clear_inline();
            }
            // The caret moved: read its new surroundings ahead of typing,
            // once it rests there.
            let mut delay = warm_delay(key);
            if delay == INLINE_WARM_BROWSING && self.last_edit_time.elapsed() > INLINE_RECENT_EDIT {
                delay = INLINE_WARM_READING;
            }
            self.schedule_inline_warm(target, delay);
            return;
        }
        let remainder = self.vtcomplete_inline.ghost.take().and_then(|ghost| {
            let editor = self.editor()?;
            Some((typed_along(&ghost, &target, &editor.rope)?, ghost.retype))
        });
        if !trigger {
            self.vtcomplete_inline.cancel();
            return;
        }
        if let Some((text, retype)) = remainder {
            let rope = self.editor().unwrap().rope.clone();
            self.vtcomplete_inline.ghost = Some(Ghost {
                target: target.clone(),
                rope,
                text,
                retype,
            });
        }
        self.submit_inline(target, false);
    }

    /// The "Predict Here" command: ask for a prediction at the caret now,
    /// and show the best guess even when it is uncertain.
    pub(crate) fn request_inline_prediction(&mut self) {
        match self.inline_availability() {
            Availability::Enabled => {}
            Availability::Disabled => {
                self.notify("GPTComplete is turned off");
                return;
            }
            Availability::Snoozed => {
                self.notify("GPTComplete is snoozed");
                return;
            }
        }
        let Some(target) = self.inline_target() else {
            self.notify("Predictions are available while editing a file");
            return;
        };
        self.dismiss_vtcomplete();
        self.vtcomplete_inline.ghost = None;
        self.submit_inline(target, true);
    }

    fn clear_inline(&mut self) {
        self.vtcomplete_inline.ghost = None;
        self.vtcomplete_inline.cancel();
    }

    fn submit_inline(&mut self, target: Target, explicit: bool) {
        self.vtcomplete_inline.suppressed = None;
        self.vtcomplete_inline.requested = Some(target.clone());
        // A popup already in view stays; otherwise it waits for the answer.
        let hold = !self.vtcomplete_inline.popup_drawn;
        self.submit_inline_job(target, explicit, false);
        if hold {
            self.vtcomplete_inline.holding = Some(std::time::Instant::now());
        }
    }

    /// Whether the word popup is held back for a pending prediction.
    pub(crate) fn inline_holds_popup(&self) -> bool {
        self.vtcomplete_inline
            .holding
            .is_some_and(|since| since.elapsed() < INLINE_HOLD)
            && !self.vtcomplete_explicit
            && !self.vtcomplete_navigated
    }

    /// Time until a hold on the word popup runs out, for the event loop.
    pub(crate) fn inline_hold_wait(&self) -> Option<std::time::Duration> {
        let since = self.vtcomplete_inline.holding?;
        Some(INLINE_HOLD.saturating_sub(since.elapsed()))
    }

    /// End a hold that ran out without an answer. True when the display
    /// changes.
    pub(crate) fn expire_inline_hold(&mut self) -> bool {
        let expired = self
            .vtcomplete_inline
            .holding
            .is_some_and(|since| since.elapsed() >= INLINE_HOLD);
        if expired {
            self.vtcomplete_inline.holding = None;
        }
        expired && self.vtcomplete_active
    }

    /// Whether the word popup is out of view: covered by a prediction, or
    /// held back for one.
    pub(crate) fn inline_hides_popup(&self) -> bool {
        self.inline_ghost_visible() || self.inline_holds_popup()
    }

    /// A click placed the caret: read ahead there soon, as typing usually
    /// follows.
    pub(crate) fn note_inline_pointer_caret(&mut self) {
        if !self.quick_suggestions {
            return;
        }
        if let Some(target) = self.inline_target() {
            self.schedule_inline_warm(target, INLINE_WARM_PRECISE);
        }
    }

    /// Read ahead at `target` once the caret has rested there for `delay`.
    /// A reading for the previous position stops now.
    fn schedule_inline_warm(&mut self, target: Target, delay: std::time::Duration) {
        if self.vtcomplete_inline.ghost.is_some()
            || self.vtcomplete_inline.warmed.as_ref() == Some(&target)
        {
            return;
        }
        self.vtcomplete_inline.cancel();
        self.vtcomplete_inline.warm_due = Some((target, Instant::now() + delay));
    }

    /// Time until a resting caret is due to be read ahead, for the event loop.
    pub(crate) fn inline_warm_wait(&self, now: Instant) -> Option<std::time::Duration> {
        let (_, due) = self.vtcomplete_inline.warm_due.as_ref()?;
        Some(due.saturating_duration_since(now))
    }

    /// Read ahead at the caret once it has rested. Nothing is drawn.
    pub(crate) fn start_due_inline_warm(&mut self, now: Instant) {
        if self
            .vtcomplete_inline
            .warm_due
            .as_ref()
            .is_none_or(|(_, due)| *due > now)
        {
            return;
        }
        let (target, _) = self.vtcomplete_inline.warm_due.take().unwrap();
        if self.inline_target().as_ref() == Some(&target)
            && self.vtcomplete_inline.ghost.is_none()
            && self.vtcomplete_inline.warmed.as_ref() != Some(&target)
        {
            self.vtcomplete_inline.warmed = Some(target.clone());
            self.submit_inline_job(target, false, true);
        }
    }

    fn submit_inline_job(&mut self, target: Target, explicit: bool, warm: bool) {
        self.vtcomplete_inline.cancel();
        self.vtcomplete_inline.warm_due = None;
        let Some(editor) = self.editor() else {
            return;
        };
        let syntax = Syntax::for_language(editor.language_name());
        let language = crate::vtcomplete_inline::language_id(editor.language_name());
        let rope = editor.rope.clone();
        let options = self.completion_preferences.inline_eagerness.options();
        // An unoptimized test build needs far longer for the language model.
        #[cfg(test)]
        let options = Options {
            budget: if self.vtcomplete_inline.with_language_model {
                std::time::Duration::from_secs(120)
            } else {
                options.budget
            },
            ..options
        };
        let cancel = CancellationToken::new();
        let generation = self.vtcomplete_inline.generation;
        let job = Job {
            target,
            rope,
            syntax,
            language,
            explicit,
            warm,
            options,
            generation,
            #[cfg(test)]
            with_language_model: self.vtcomplete_inline.with_language_model,
            cancel: cancel.clone(),
            sender: self.background.sender.clone(),
            activity: self.vtcomplete_worker.activity().clone(),
        };
        self.vtcomplete_inline.cancel = Some(cancel);
        #[cfg(test)]
        if !warm {
            self.vtcomplete_inline.submissions += 1;
        }
        let pool = self.background.completion_sender();
        self.vtcomplete_inline.submit(job, pool);
    }

    /// Deliver a worker result. Returns whether the display changed.
    pub(crate) fn receive_inline(
        &mut self,
        generation: u64,
        target: Target,
        rope: ropey::Rope,
        text: Option<String>,
    ) -> bool {
        if generation != self.vtcomplete_inline.generation
            || self.inline_target().as_ref() != Some(&target)
            || self.vtcomplete_inline.suppressed.as_ref() == Some(&target)
            || !self
                .editor()
                .is_some_and(|editor| editor.rope.is_instance(&rope))
        {
            return false;
        }
        // The worker can still be reading ahead after delivering the result.
        // Keep its cancellation token so snooze and disable stop that work too.
        // The answer ends a hold on the word popup, which then shows if no
        // prediction covers it.
        let held = self.vtcomplete_inline.holding.take().is_some();
        // No answer leaves the remainder of a prediction being typed along.
        let Some(text) = text else {
            return held && self.vtcomplete_active;
        };
        let changed = self
            .vtcomplete_inline
            .ghost
            .as_ref()
            .is_none_or(|ghost| ghost.text != text || ghost.target != target);
        let retype = {
            let mut queue = self
                .vtcomplete_inline
                .queue
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            queue
                .retype
                .take_if(|(answered, _)| *answered == generation)
                .and_then(|(_, retype)| {
                    Some((target.cursor.1.checked_sub(retype.chars().count())?, retype))
                })
        };
        self.vtcomplete_inline.ghost = Some(Ghost {
            target,
            rope,
            text,
            retype,
        });
        changed
    }

    /// An explicitly opened or navigated popup has priority over ghost text.
    pub(crate) fn inline_ghost_visible(&self) -> bool {
        if self.vtcomplete_explicit
            || self.vtcomplete_navigated
            || crate::ui_lsp_completion::visible(self)
        {
            return false;
        }
        self.vtcomplete_inline.ghost.as_ref().is_some_and(|ghost| {
            self.inline_target().as_ref() == Some(&ghost.target)
                && self
                    .editor()
                    .is_some_and(|editor| editor.rope.is_instance(&ghost.rope))
        })
    }

    #[cfg(test)]
    pub(crate) fn inline_ghost_text(&self) -> Option<&str> {
        self.inline_ghost_visible()
            .then(|| self.vtcomplete_inline.ghost.as_ref().unwrap().text.as_str())
    }

    /// Hide the visible prediction until the text changes again.
    pub(crate) fn dismiss_inline(&mut self) -> bool {
        if !self.inline_ghost_visible() {
            return false;
        }
        self.vtcomplete_inline.suppressed = self.inline_target();
        self.clear_inline();
        true
    }

    /// Insert the visible prediction, or only its next word.
    pub(crate) fn accept_inline(&mut self, word: bool) -> bool {
        if !self.inline_ghost_visible() {
            return false;
        }
        let Ghost {
            text,
            retype,
            target: shown,
            ..
        } = self.vtcomplete_inline.ghost.take().unwrap();
        let end = if word {
            next_word_end(&text)
        } else {
            text.len()
        };
        self.vtcomplete_inline.cancel();
        self.dismiss_vtcomplete();
        let previous = self.file_search_edit_snapshot();
        self.pin_preview_tab();
        self.record_edit_position();
        if let Some(editor) = self.editor_mut() {
            // The typed start of the word takes the prediction's spelling:
            // it is selected, so that the paste replaces it.
            let mut inserted = text[..end].to_owned();
            if let Some((column, spelling)) = retype
                && editor.cursor_line == shown.cursor.0
                && editor.selection_anchor.is_none()
                && column + spelling.chars().count() <= editor.cursor_col
            {
                let line = editor.rope.line(editor.cursor_line);
                let typed: String = line.slice(column..editor.cursor_col).chars().collect();
                let length = spelling.len();
                if typed.len() >= length
                    && typed.is_char_boundary(length)
                    && typed[..length].eq_ignore_ascii_case(&spelling)
                {
                    inserted = format!("{spelling}{}{inserted}", &typed[length..]);
                    editor.selection_anchor = Some((editor.cursor_line, column));
                }
            }
            // One editor transaction and one undo step, with the exact text.
            editor.paste_snapshot_text(&inserted, false, Some(false));
        }
        if let Some(previous) = previous
            && self.refresh_document_feedback_after_edit(&previous)
        {
            if let Some(editor) = self.editor_mut() {
                editor.mark_syntax_dirty_range(
                    editor.earliest_edit_line(),
                    editor.latest_edit_line(),
                );
            }
            self.refresh_search_after_edit(&previous);
        }
        let Some(target) = self.inline_target() else {
            return true;
        };
        if end < text.len() {
            let rope = self.editor().unwrap().rope.clone();
            self.vtcomplete_inline.ghost = Some(Ghost {
                target,
                rope,
                text: text[end..].to_owned(),
                retype: None,
            });
        } else if self.quick_suggestions {
            // The prediction made ahead for exactly this text shows at once.
            let speculation = self
                .vtcomplete_inline
                .queue
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .speculation
                .take();
            if let Some(speculation) = speculation
                && speculation.document == target.document
                && speculation.cursor == target.cursor
                && let Some(editor) = self.editor()
                && editor.rope == speculation.rope
            {
                let rope = editor.rope.clone();
                self.vtcomplete_inline.ghost = Some(Ghost {
                    target: target.clone(),
                    rope,
                    text: speculation.text,
                    retype: None,
                });
            }
            // Continue from the accepted text without another keystroke.
            self.submit_inline(target, false);
        }
        true
    }
}

/// The remainder of a prediction after the user typed its beginning.
fn typed_along(ghost: &Ghost, target: &Target, rope: &ropey::Rope) -> Option<String> {
    let (line, column) = ghost.target.cursor;
    if target.document != ghost.target.document
        || target.group != ghost.target.group
        || target.tab != ghost.target.tab
        || target.cursor.0 != line
        || target.cursor.1 <= column
    {
        return None;
    }
    let typed = target.cursor.1 - column;
    if rope.len_chars() != ghost.rope.len_chars() + typed || line >= rope.len_lines() {
        return None;
    }
    let source = rope.line(line);
    if target.cursor.1 > source.len_chars() {
        return None;
    }
    let mut remainder = ghost.text.as_str();
    for ch in source.slice(column..target.cursor.1).chars() {
        remainder = remainder.strip_prefix(ch)?;
    }
    (!remainder.is_empty()).then(|| remainder.to_owned())
}

/// Include leading separators with the following word. Every acceptance
/// advances, even for a tail holding only punctuation.
fn next_word_end(text: &str) -> usize {
    let mut in_word = false;
    for (at, ch) in text.char_indices() {
        let word = ch.is_alphanumeric() || ch == '_' || ch == '$';
        if in_word && !word {
            return at;
        }
        if ch == '\n' && at > 0 {
            return at;
        }
        in_word |= word;
    }
    text.len()
}

/// Paint the prediction after the caret and shift the visible rest of the
/// line, without touching the source. Further predicted lines push the rows
/// below down, as accepting the prediction would.
pub(crate) fn draw_ghost(frame: &mut ratatui::Frame, app: &mut App) {
    use ratatui::{
        layout::Rect,
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };
    if !app.inline_ghost_visible() {
        return;
    }
    let Some(area) = app.code_area else {
        return;
    };
    let Some((x, y)) = ghost_cursor_position(app, area) else {
        return;
    };
    if !frame.area().contains((x, y).into()) {
        return;
    }
    let editor = app.editor().unwrap();
    let ghost = app.vtcomplete_inline.ghost.as_ref().unwrap();
    let line = editor.rope.line(editor.cursor_line);
    let suffix: String = line
        .chars_at(editor.cursor_col.min(line.len_chars()))
        .take(area.width as usize)
        .take_while(|c| *c != '\n' && *c != '\r')
        .collect();
    // The caret cell changes background on each blink; sample the row's
    // steady background so the suggestion does not flash with it.
    let base = crate::merge_conflict_ui::content_background(
        editor,
        editor.cursor_line,
        &app.colors,
        app.colors.bg_line_highlight,
    );
    let gradient = if base == app.colors.bg_editor {
        app.colors.gradients.bg_editor
    } else {
        app.colors.gradients.bg_line_highlight
    };
    let projection = ((base == app.colors.bg_editor || base == app.colors.bg_line_highlight)
        && gradient.start != gradient.end)
        .then(|| {
            crate::app_theme::GradientPrecomputed::new(area.width, area.height, gradient.angle_deg)
        });
    let right = area.right().min(frame.area().right());
    let bottom = area.bottom().min(frame.area().bottom());
    let background = |column: u16, row: u16| {
        projection.as_ref().map_or(base, |projection| {
            crate::app_theme::lerp_color(
                gradient.start,
                gradient.end,
                projection.t_at(column - area.x, row - area.y),
            )
        })
    };
    let style = Style::default()
        .remove_modifier(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK | Modifier::UNDERLINED);
    let ghost_style = style.fg(app.colors.fg_dim).add_modifier(Modifier::ITALIC);
    let size = editor.tab_size.max(1);
    let mut lines = ghost.text.split('\n');
    let first = lines.next().unwrap_or("");
    let count = ghost.text.matches('\n').count();
    let multiline = count > 0;
    // Further predicted lines get rows of their own: the rendered rows below
    // the caret, gutter included, move down to make room for them.
    let left = app
        .gutter_area
        .filter(|gutter| gutter.right() <= area.x && gutter.y <= y && gutter.bottom() > y)
        .map_or(area.x, |gutter| gutter.x);
    if multiline {
        push_rows_down(frame.buffer_mut(), left, right, y + 1, bottom, count as u16);
        for row in y + 1..(y + 1 + count as u16).min(bottom) {
            for column in left..area.x {
                let bg = frame.buffer_mut()[(column, y)].bg;
                let cell = &mut frame.buffer_mut()[(column, row)];
                cell.reset();
                cell.set_bg(bg);
            }
        }
    }
    for column in x..right {
        let cell = &mut frame.buffer_mut()[(column, y)];
        if multiline {
            // The rest of the caret line moves to the last predicted line.
            cell.set_symbol(" ");
        }
        cell.set_bg(background(column, y));
    }
    let mut phase = size
        - crate::editor_columns::tab_width_at(
            editor.rope.line(editor.cursor_line),
            editor.cursor_col,
            size,
        );
    let ghost_text = expand_tabs(first, &mut phase, size);
    let suffix_text = if multiline {
        String::new()
    } else {
        expand_tabs(&suffix, &mut phase, size)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(ghost_text, ghost_style),
            Span::styled(suffix_text, style.fg(app.colors.fg_text)),
        ]))
        .style(style),
        Rect::new(x, y, right.saturating_sub(x), 1),
    );
    // Following lines fill the rows made free; the last one carries the text
    // that was right of the caret.
    for (index, text) in lines.enumerate() {
        let row = y + 1 + index as u16;
        if row >= bottom {
            break;
        }
        for column in area.x..right {
            let cell = &mut frame.buffer_mut()[(column, row)];
            cell.reset();
            cell.set_bg(background(column, row));
        }
        let mut phase = 0;
        let mut spans = vec![Span::styled(
            expand_tabs(text, &mut phase, size),
            ghost_style,
        )];
        if index + 1 == count {
            spans.push(Span::styled(
                expand_tabs(&suffix, &mut phase, size),
                style.fg(app.colors.fg_text),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(style),
            Rect::new(area.x, row, right.saturating_sub(area.x), 1),
        );
    }
    // A small underline caret may blink; the suggestion's glyphs and colors
    // stay identical, including its first character.
    if app.cursor_visible {
        frame.buffer_mut()[(x, y)]
            .modifier
            .insert(Modifier::UNDERLINED);
    }
}

/// Move the rows `top..bottom` of the columns `left..right` down by `by`
/// rows; rows pushed past `bottom` drop out of view.
fn push_rows_down(
    buffer: &mut ratatui::buffer::Buffer,
    left: u16,
    right: u16,
    top: u16,
    bottom: u16,
    by: u16,
) {
    let area = buffer.area;
    let (left, right) = (left.max(area.x), right.min(area.right()));
    let bottom = bottom.min(area.bottom());
    if left >= right || by == 0 {
        return;
    }
    let width = (right - left) as usize;
    // One slice copy per row, reusing each cell's storage: no allocation.
    for row in (top.saturating_add(by)..bottom).rev() {
        let source = buffer.index_of(left, row - by);
        let target = buffer.index_of(left, row);
        let (above, below) = buffer.content.split_at_mut(target);
        below[..width].clone_from_slice(&above[source..source + width]);
    }
}

fn expand_tabs(text: &str, column: &mut usize, size: usize) -> String {
    let mut result = String::new();
    for ch in text.chars() {
        if ch == '\t' {
            result.extend(std::iter::repeat_n(' ', size - *column % size));
            *column = 0;
        } else {
            result.push(ch);
            *column += 1;
        }
    }
    result
}

/// Reuse the editor's actual source-to-cell mapping, folds and wrapped rows.
/// Returning None for offscreen carets prevents a clamped phantom suggestion.
fn ghost_cursor_position(app: &mut App, area: ratatui::layout::Rect) -> Option<(u16, u16)> {
    let sticky = if app.sticky_scroll {
        app.cached_sticky_lines.len()
    } else {
        0
    };
    let editor = app.editor_mut()?;
    let (line, col) = (editor.cursor_line, editor.cursor_col);
    let (row, start) = if editor.word_wrap {
        let width = editor.effective_wrap_width();
        let rows = editor.wrapped_rows(editor.wrap_scroll_offset, area.height as usize, width);
        let (row, info) = rows.iter().enumerate().find(|(_, info)| {
            info.line_idx == line
                && col >= info.start_col
                && (col < info.end_col || col == info.line_len && col == info.end_col)
        })?;
        (row, info.start_col)
    } else {
        let row = editor
            .visible_lines(editor.scroll_offset, area.height as usize)
            .iter()
            .position(|&visible| visible == line)?;
        (row, editor.horizontal_scroll)
    };
    if col < start || row < sticky && editor.scroll_offset > 0 {
        return None;
    }
    let cells = editor.cell_at_source_column(line, start, col);
    if cells >= area.width as usize {
        return None;
    }
    Some((area.x + cells as u16, area.y + row as u16))
}

#[cfg(test)]
#[path = "app_vtcomplete_inline_tests.rs"]
mod tests;
