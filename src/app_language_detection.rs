//! Demand-driven fallback language detection. One running/one newest pending
//! job, bounded immutable snapshots, and no idle timer once checked.
use crate::{
    app::{App, TabKind},
    background::{
        CancellationToken,
        latest::{Job, LatestWork},
    },
    editor::Editor,
    language_detection::{self, Binding, Observation, Snapshot},
    textmate::SyntaxRegistry,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const EDIT_QUIET: Duration = Duration::from_millis(350);
const MIN_INTERVAL: Duration = Duration::from_millis(250);

impl Observation {
    fn capture(editor: &Editor, registry: u64) -> Self {
        Self {
            epoch: editor.language_detection_epoch,
            revision: editor.document_revision(),
            version: editor.content_version,
            path: editor.syntax_file_path().map(ToOwned::to_owned),
            registry,
            manual: editor.language_override.clone(),
            known: false,
        }
    }
    fn same_identity(&self, editor: &Editor, registry: u64) -> bool {
        self.epoch == editor.language_detection_epoch
            && self.registry == registry
            && self.path.as_deref() == editor.syntax_file_path()
            && self.manual == editor.language_override
    }
    fn current(&self, editor: &Editor, registry: u64) -> bool {
        self.same_identity(editor, registry)
            && self.revision == editor.document_revision()
            && self.version == editor.content_version
    }
    fn checked(&self, editor: &Editor, registry: u64) -> bool {
        self.same_identity(editor, registry)
            && (self.known
                || self.current(editor, registry)
                || editor.inferred_language.as_ref().is_some_and(|binding| {
                    binding.registry == registry
                        && binding.path.as_deref() == editor.syntax_file_path()
                }))
    }
}
struct Request {
    view: u64,
    observation: Observation,
    snapshot: Snapshot,
    syntaxes: Arc<SyntaxRegistry>,
}
struct Reply {
    view: u64,
    observation: Observation,
    language: Option<String>,
    #[cfg(test)]
    worker: std::thread::ThreadId,
}
#[derive(Default)]
struct SampleCache {
    hash: u64,
    registry: u64,
    language: Option<String>,
    initialized: bool,
}
#[derive(Default)]
pub(crate) struct Detector {
    work: LatestWork,
    observed: Option<(u64, Observation)>,
    queued: Option<Request>,
    due: Option<Instant>,
    last_dispatch: Option<Instant>,
    reply: Arc<Mutex<Option<Reply>>>,
    cache: Arc<Mutex<SampleCache>>,
}
impl Detector {
    fn cancel(&mut self) {
        if self.observed.take().is_some() || self.queued.is_some() {
            self.work.cancel();
        }
        self.queued = None;
        self.due = None;
    }
}

impl App {
    pub(crate) fn language_detection_wait(&self, now: Instant) -> Option<Duration> {
        self.language_detector
            .due
            .map(|due| due.saturating_duration_since(now))
    }

    pub(crate) fn poll_language_detection(&mut self, now: Instant) -> bool {
        let registry = self.highlighter.registry_generation();
        let reply = self
            .language_detector
            .reply
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        let mut changed = false;
        if let Some(reply) = reply {
            if let Some(editor) = self.editor_mut().filter(|editor| {
                editor.navigation_id == reply.view
                    && !editor.bg_loading
                    && !editor.load_failed
                    && editor.encoding != "Binary"
                    && reply.observation.current(editor, registry)
                    && editor.explicit_language_token().is_none()
            }) {
                let previous = editor.syntax_language_token().map(str::to_owned);
                editor.inferred_language = reply.language.map(|syntax| Binding {
                    syntax,
                    path: reply.observation.path,
                    registry,
                });
                editor.publish_document();
                let mut checked = Observation::capture(editor, registry);
                checked.known = reply.observation.known;
                editor.language_detection = Some(checked);
                changed = previous.as_deref() != editor.syntax_language_token();
            }
        }
        if changed {
            self.invalidate_visible_highlights();
        }
        let eligible = self.tabs.get(self.active_tab).is_some_and(|tab| {
            matches!(
                tab.kind,
                TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails
            ) && !tab.editor.bg_loading
                && !tab.editor.load_failed
                && tab.editor.encoding != "Binary"
        });
        if !eligible {
            self.language_detector.cancel();
            return changed;
        }
        let stale = self
            .editor()
            .unwrap()
            .inferred_language
            .as_ref()
            .is_some_and(|binding| {
                binding.path.as_deref() != self.editor().unwrap().syntax_file_path()
            });
        if stale {
            let editor = self.editor_mut().unwrap();
            let previous = editor.syntax_language_token().map(str::to_owned);
            editor.inferred_language = None;
            editor.language_detection = None;
            editor.publish_document();
            if previous.as_deref() != editor.syntax_language_token() {
                self.invalidate_visible_highlights();
                changed = true;
            }
        }
        // Keep the previous inferred mode while a registry/theme reload is
        // checked in the background, avoiding a flash of unhighlighted text.
        let editor = self.editor().unwrap();
        if editor
            .language_detection
            .as_ref()
            .is_some_and(|checked| checked.checked(editor, registry))
            || editor.inferred_language.as_ref().is_some_and(|binding| {
                binding.registry == registry && binding.path.as_deref() == editor.syntax_file_path()
            })
        {
            self.language_detector.cancel();
            return changed;
        }
        // Initialize the shared-document revision before taking a request key.
        // No content is flattened or classified on the UI thread.
        if editor.document_revision() == 0 {
            self.editor_mut().unwrap().publish_document();
        }
        let editor = self.editor().unwrap();
        let same = self
            .language_detector
            .observed
            .as_ref()
            .is_some_and(|(view, observation)| {
                *view == editor.navigation_id && observation.current(editor, registry)
            });
        if !same {
            if editor.explicit_language_token().is_some() {
                let mut checked = Observation::capture(editor, registry);
                checked.known = true;
                self.editor_mut().unwrap().language_detection = Some(checked);
                self.language_detector.cancel();
                return changed;
            }

            let same_document =
                self.language_detector
                    .observed
                    .as_ref()
                    .is_some_and(|(view, observation)| {
                        *view == editor.navigation_id
                            && observation.path.as_deref() == editor.syntax_file_path()
                    })
                    || editor
                        .language_detection
                        .as_ref()
                        .is_some_and(|observation| observation.same_identity(editor, registry));
            let observation = Observation::capture(editor, registry);
            let view = editor.navigation_id;
            let request = Request {
                view,
                observation: observation.clone(),
                snapshot: Snapshot::capture(&editor.rope),
                syntaxes: self.highlighter.shared_sets().0,
            };
            self.language_detector.work.cancel();
            self.language_detector.observed = Some((view, observation));
            self.language_detector.queued = Some(request);
            let quiet = if same_document { now + EDIT_QUIET } else { now };
            self.language_detector.due = Some(
                self.language_detector
                    .last_dispatch
                    .map_or(quiet, |last| quiet.max(last + MIN_INTERVAL)),
            );
        }
        if self.language_detector.due.is_some_and(|due| now >= due) {
            let request = self.language_detector.queued.take().unwrap();
            self.language_detector.due = None;
            self.language_detector.last_dispatch = Some(now);
            let reply = self.language_detector.reply.clone();
            let cache = self.language_detector.cache.clone();
            let cancel = CancellationToken::new();
            let worker_cancel = cancel.clone();
            self.language_detector.work.replace(
                self.background.pool_sender(),
                Job {
                    cancel,
                    run: Box::new(move || {
                        let result = detect(request, &cache, &worker_cancel);
                        if !worker_cancel.is_cancelled() {
                            if let Some(result) = result {
                                *reply.lock().unwrap_or_else(|error| error.into_inner()) =
                                    Some(result);
                            }
                        }
                    }),
                },
            );
        }
        changed
    }
}

fn detect(
    mut request: Request,
    cache: &Mutex<SampleCache>,
    cancel: &CancellationToken,
) -> Option<Reply> {
    if cancel.is_cancelled() {
        return None;
    }
    let path = request.observation.path.as_deref();
    let decoded_name = path.and_then(crate::remote::workspace::display_name);
    let filename = decoded_name.as_deref().or_else(|| {
        path.and_then(std::path::Path::file_name)
            .and_then(|name| name.to_str())
    });
    let extension = filename
        .map(std::path::Path::new)
        .and_then(std::path::Path::extension)
        .and_then(|name| name.to_str());
    let known = filename
        .and_then(|name| request.syntaxes.find_syntax_by_extension(name))
        .or_else(|| extension.and_then(|ext| request.syntaxes.find_syntax_by_extension(ext)));
    let language = if known.is_some()
        || extension
            .is_some_and(|ext| ext.eq_ignore_ascii_case("txt") || ext.eq_ignore_ascii_case("text"))
    {
        request.observation.known = true;
        // The registry also supplies the label for languages beyond the
        // editor's legacy extension table, without running the model.
        known
            .filter(|syntax| syntax.name != "Plain Text")
            .map(|syntax| syntax.name.clone())
    } else {
        let sample = request.snapshot.sample(cancel)?;
        let hash = sample.fingerprint();
        let cached = cache.lock().unwrap_or_else(|error| error.into_inner());
        if cached.initialized
            && cached.hash == hash
            && cached.registry == request.observation.registry
        {
            cached.language.clone()
        } else {
            drop(cached);
            let explicit = request
                .syntaxes
                .find_syntax_by_first_line(&sample.first_line());
            let syntax = explicit.or_else(|| {
                let info = language_detection::classify(&sample, cancel)?;
                request
                    .syntaxes
                    .find_syntax_by_token(info.label)
                    .or_else(|| {
                        info.extensions
                            .iter()
                            .find_map(|ext| request.syntaxes.find_syntax_by_extension(ext))
                    })
            });
            let language = syntax
                .filter(|syntax| syntax.name != "Plain Text")
                .map(|syntax| syntax.name.clone());
            if cancel.is_cancelled() {
                return None;
            }
            *cache.lock().unwrap_or_else(|error| error.into_inner()) = SampleCache {
                hash,
                registry: request.observation.registry,
                language: language.clone(),
                initialized: true,
            };
            language
        }
    };
    Some(Reply {
        view: request.view,
        observation: request.observation,
        language,
        #[cfg(test)]
        worker: std::thread::current().id(),
    })
}

#[cfg(test)]
#[path = "app_language_detection_tests.rs"]
mod tests;
