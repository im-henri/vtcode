//! Remote clipboard qualification and conditional input resolution. Metadata,
//! validation and replay never run on the input/render thread. A session has at
//! most one worker; continued typing reuses results without repeating RPCs.
use super::*;
use crate::text_input::{DeferredInput, FrozenInput, ResolvedInput};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PasteContext {
    root: PathBuf,
    client: remote::client::Client,
}
impl PasteContext {
    fn matches(&self, workspace: &remote::workspace::Workspace) -> bool {
        self.root == workspace.view_root && self.client.same_connection(&workspace.client)
    }
}
pub(crate) struct Job {
    serial: u64,
    stamp: (u64, usize),
    context: PasteContext,
    epoch: Option<u64>,
    cancel: CancellationToken,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
struct Seed {
    base: Vec<PathBuf>,
    files: Vec<PathBuf>,
    decisions: HashMap<u64, bool>,
    error: Option<Option<String>>,
}
impl Seed {
    fn new(files: Vec<PathBuf>) -> Self {
        Self {
            base: files.clone(),
            files,
            decisions: HashMap::new(),
            error: None,
        }
    }
}
pub(crate) struct Batch {
    seed: Seed,
    resolved: Option<ResolvedInput>,
}

#[derive(Clone)]
pub(crate) struct FrozenPaste {
    input: Arc<FrozenInput>,
    context: PasteContext,
    prior_error: Option<String>,
}
impl std::fmt::Debug for FrozenPaste {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenPaste")
            .field("input", &self.input)
            .field("root", &self.context.root)
            .finish()
    }
}
pub(crate) struct ResolvedDraft {
    pub input: ResolvedInput,
    pub files: Vec<PathBuf>,
    pub error: Option<String>,
}

pub(super) fn prepare_prompt(
    workspace: &remote::workspace::Workspace,
    input: String,
    attached: Vec<PathBuf>,
    buffers: Vec<Buffer>,
    embedded: bool,
    cancel: &CancellationToken,
    paste: Option<Arc<FrozenPaste>>,
) -> (Result<PreparedPrompt, String>, Option<ResolvedDraft>) {
    let Some(paste) = paste else {
        return (
            prompt(workspace, input, attached, buffers, embedded, cancel),
            None,
        );
    };
    if !paste.context.matches(workspace) {
        return (
            Err("The remote workspace changed. Review the pasted paths before sending.".into()),
            None,
        );
    }
    let Some((batch, mut cache)) = qualify(
        workspace,
        &paste.input.replay,
        &buffers,
        Seed::new(attached),
        cancel,
    ) else {
        return (Err("Message preparation cancelled.".into()), None);
    };
    let Some(resolved) = batch
        .resolved
        .or_else(|| paste.input.replay.resolve(&batch.seed.decisions, cancel))
    else {
        return (
            Err("The message changed during file context preparation.".into()),
            None,
        );
    };
    let result = if resolved.text.trim().is_empty() && batch.seed.files.is_empty() {
        Ok(PreparedPrompt {
            content: Vec::new(),
            display: resolved.text.clone(),
            source: None,
        })
    } else {
        prompt_cached(
            workspace,
            resolved.text.clone(),
            batch.seed.files.clone(),
            buffers,
            embedded,
            cancel,
            &mut cache,
        )
    };
    (
        result,
        Some(ResolvedDraft {
            input: resolved,
            files: batch.seed.files,
            error: batch
                .seed
                .error
                .unwrap_or_else(|| paste.prior_error.clone()),
        }),
    )
}

fn qualify(
    workspace: &remote::workspace::Workspace,
    replay: &DeferredInput,
    buffers: &[Buffer],
    mut seed: Seed,
    cancel: &CancellationToken,
) -> Option<(Batch, PreparationCache)> {
    let mut cache = PreparationCache::default();
    for (id, text) in replay.candidates() {
        if cancel.is_cancelled() {
            return None;
        }
        if seed.decisions.contains_key(&id) {
            continue;
        }
        // Like native is_file(), unavailable or invalid paths stay ordinary
        // pasted text. Existing non-text files consume the paste and report
        // their attachment error without replacing the original selection.
        let paths = pasted_remote_paths(workspace, &text, &mut cache, cancel)
            .ok()
            .flatten();
        seed.decisions.insert(id, paths.is_some());
        if let Some(paths) = paths {
            let candidates = seed.files.iter().cloned().chain(paths).collect();
            match prepare_files_cached(workspace, candidates, buffers, false, cancel, &mut cache) {
                Ok(prepared) => {
                    seed.files = prepared.files.into_iter().map(|(path, _)| path).collect();
                    seed.error = Some(None);
                }
                Err(error) => seed.error = Some(Some(error)),
            }
        }
    }
    if cancel.is_cancelled() {
        return None;
    }
    let resolved = if seed.decisions.values().any(|consumed| *consumed) {
        replay.resolve(&seed.decisions, cancel)
    } else {
        None
    };
    Some((Batch { seed, resolved }, cache))
}

impl App {
    pub(crate) fn freeze_remote_chat_paste(&mut self) -> Option<Arc<FrozenPaste>> {
        let target = self.chat.selected.clone();
        let job = self.chat.files.pasting.remove(&target)?;
        if self
            .remote_workspace
            .as_ref()
            .is_none_or(|workspace| !job.context.matches(workspace))
            || self.chat_file_root() != job.context.root
        {
            return None;
        }
        let input = self
            .chat
            .with_composer_input(|text, cursor, anchor, history| {
                std::mem::take(history).freeze(text, *cursor, *anchor)
            })?;
        if input.replay.stamp().0 != job.stamp.0 {
            return None;
        }
        Some(Arc::new(FrozenPaste {
            input: Arc::new(input),
            context: job.context.clone(),
            prior_error: self.chat.error.clone(),
        }))
    }
    pub(crate) fn restore_remote_chat_paste(&mut self, paste: Arc<FrozenPaste>) {
        if self
            .remote_workspace
            .as_ref()
            .is_none_or(|workspace| !paste.context.matches(workspace))
            || self.chat_file_root() != paste.context.root
        {
            return;
        }
        let resolved = paste.input.restore();
        self.chat
            .with_composer_input(|text, cursor, anchor, history| {
                *text = resolved.text;
                *cursor = resolved.cursor;
                *anchor = resolved.anchor;
                *history = resolved.history;
            });
        if let Some(replay) = self.paste_snapshot() {
            self.start_chat_paste_work(
                replay,
                paste.context.clone(),
                Seed::new(self.chat.files.attached.clone()),
            );
        }
    }
    fn paste_snapshot(&mut self) -> Option<DeferredInput> {
        self.chat
            .with_composer_input(|text, cursor, anchor, history| {
                history.deferred_input(text, *cursor, *anchor).cloned()
            })
    }
    fn paste_stamp(&mut self) -> Option<(u64, usize)> {
        self.chat
            .with_composer_input(|_, _, _, history| history.deferred_stamp())
    }
    fn cancel_paste_input(&mut self, id: u64) {
        self.chat
            .with_composer_input(|text, cursor, anchor, history| {
                if history
                    .deferred_input(text, *cursor, *anchor)
                    .is_some_and(|replay| replay.stamp().0 == id)
                {
                    history.break_group();
                }
            });
    }
    pub(crate) fn defer_remote_chat_file_paste(&mut self, text: &str) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        let Some(workspace) = self
            .remote_workspace
            .clone()
            .filter(|workspace| workspace.view_root == self.chat_file_root())
        else {
            return false;
        };
        let target = self.chat.selected.clone();
        if let Some(stamp) = self.paste_stamp()
            && self
                .chat
                .files
                .pasting
                .get(&target)
                .is_none_or(|job| job.stamp.0 != stamp.0 || !job.context.matches(&workspace))
        {
            self.cancel_paste_input(stamp.0);
        }
        self.chat.files.work_serial = self.chat.files.work_serial.wrapping_add(1);
        let id = self.chat.files.work_serial;
        self.chat
            .with_composer_input(|text_input, cursor, anchor, history| {
                history.conditional_paste(
                    TextInput::new(text_input, cursor, anchor).multiline(true),
                    id,
                    text,
                );
            });
        self.chat.input_goal = None;
        self.chat.focus = None;
        let Some(replay) = self.paste_snapshot() else {
            self.chat.files.pasting.remove(&target);
            return true;
        };
        if self
            .chat
            .files
            .pasting
            .get(&target)
            .is_some_and(|job| job.stamp.0 != replay.stamp().0 || !job.context.matches(&workspace))
        {
            self.chat.files.pasting.remove(&target);
        }
        if !self.chat.files.pasting.contains_key(&target) {
            let context = PasteContext {
                root: workspace.view_root.clone(),
                client: workspace.client.clone(),
            };
            self.start_chat_paste_work(
                replay,
                context,
                Seed::new(self.chat.files.attached.clone()),
            );
        }
        true
    }
    pub(crate) fn maintain_remote_chat_paste(&mut self) {
        if self.remote_workspace.is_none() && self.chat.files.pasting.is_empty() {
            return;
        }
        let target = self.chat.selected.clone();
        let stamp = self.paste_stamp();
        let valid = self.chat.files.pasting.get(&target).is_some_and(|job| {
            stamp.is_some_and(|stamp| stamp.0 == job.stamp.0)
                && self
                    .remote_workspace
                    .as_ref()
                    .is_some_and(|workspace| job.context.matches(workspace))
                && self.chat_file_root() == job.context.root
        });
        if !valid {
            self.chat.files.pasting.remove(&target);
            if let Some(stamp) = stamp {
                self.cancel_paste_input(stamp.0);
            }
        }
    }
    fn start_chat_paste_work(&mut self, replay: DeferredInput, context: PasteContext, seed: Seed) {
        let Some(workspace) = self.remote_workspace.clone().filter(|workspace| {
            context.matches(workspace) && workspace.view_root == self.chat_file_root()
        }) else {
            self.cancel_paste_input(replay.stamp().0);
            return;
        };
        let target = self.chat.selected.clone();
        let epoch = target
            .as_ref()
            .and_then(|id| self.chat.sessions.get(id))
            .map(|session| session.history_epoch);
        let needs_io = replay
            .candidates()
            .iter()
            .any(|(id, _)| !seed.decisions.contains_key(id));
        let buffers = if needs_io {
            self.remote_chat_buffers(&workspace)
        } else {
            Vec::new()
        };
        let stamp = replay.stamp();
        self.chat.files.work_serial = self.chat.files.work_serial.wrapping_add(1);
        let serial = self.chat.files.work_serial;
        let agent = self.chat.active_agent;
        let sender = self.background.sender.clone();
        let work = move |cancel: CancellationToken| {
            if let Some((result, _)) = qualify(&workspace, &replay, &buffers, seed, &cancel)
                && !cancel.is_cancelled()
            {
                let _ = sender.send(BackgroundResult::ChatPastePrepared {
                    agent,
                    serial,
                    result,
                });
            }
        };
        // A newer keystroke needs only a local replay. Use the existing CPU
        // pool for these retries, not a new thread or another metadata request.
        let cancel = if needs_io {
            self.background.submit_context("chat-paste", work)
        } else {
            self.background.submit_cancellable(work)
        };
        self.chat.files.pasting.insert(
            target,
            Job {
                serial,
                stamp,
                context,
                epoch,
                cancel,
            },
        );
    }
    pub(crate) fn apply_remote_chat_paste(&mut self, serial: u64, result: Batch) -> bool {
        let target = self
            .chat
            .files
            .pasting
            .iter()
            .find_map(|(target, job)| (job.serial == serial).then(|| target.clone()));
        let Some(target) = target else {
            return false;
        };
        let job = self.chat.files.pasting.remove(&target).unwrap();
        if self
            .remote_workspace
            .as_ref()
            .is_none_or(|workspace| !job.context.matches(workspace))
            || self.chat_file_root() != job.context.root
            || target.as_ref().is_some_and(|id| {
                self.chat
                    .sessions
                    .get(id)
                    .is_none_or(|session| Some(session.history_epoch) != job.epoch)
            })
        {
            return false;
        }
        let previous = self.chat.selected.clone();
        let visible = previous == target;
        self.chat.select_view(target);
        let changed = self.apply_selected_chat_paste(&job, result);
        self.chat.select_view(previous);
        visible && changed
    }
    fn apply_selected_chat_paste(&mut self, job: &Job, result: Batch) -> bool {
        let Some(replay) = self
            .paste_snapshot()
            .filter(|replay| replay.stamp().0 == job.stamp.0)
        else {
            return false;
        };
        if self.chat.files.attached != result.seed.base {
            self.start_chat_paste_work(
                replay,
                job.context.clone(),
                Seed::new(self.chat.files.attached.clone()),
            );
            return false;
        }
        if result.seed.decisions.values().all(|consumed| !consumed)
            && replay
                .candidates()
                .iter()
                .all(|(id, _)| result.seed.decisions.contains_key(id))
        {
            // Plain text was already edited with its real undo history. Do not
            // replay, allocate another draft, clear unrelated errors or redraw.
            self.chat
                .with_composer_input(|_, _, _, history| history.discard_deferred());
            return false;
        }
        if replay.stamp() != job.stamp || result.resolved.is_none() {
            self.start_chat_paste_work(replay, job.context.clone(), result.seed);
            return false;
        }
        let resolved = result.resolved.unwrap();
        let applied = self
            .chat
            .with_composer_input(|text, cursor, anchor, history| {
                history.apply_resolved(
                    TextInput::new(text, cursor, anchor).multiline(true),
                    job.stamp,
                    resolved,
                )
            });
        if applied {
            self.chat.files.attached = result.seed.files;
            self.chat.files.scroll = self.chat.files.attached.len().saturating_sub(3);
            if let Some(error) = result.seed.error {
                self.chat.error = error;
            }
            self.chat.input_goal = None;
            self.refresh_chat_input_layout();
        }
        applied
    }
}
