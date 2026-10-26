use crate::rope_changes::changed_lines;
#[cfg(test)]
use std::sync::Weak;
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;

use crate::background::{
    BackgroundResult, BackgroundResultSender, BackgroundTaskSender, CancellationToken,
};

use super::{VtCompleteCandidate, language_keywords};

#[path = "completion_edit_history.rs"]
mod edit_history;
#[path = "completion_numbers.rs"]
mod numbers;
#[path = "completion_patterns.rs"]
mod patterns;
#[path = "vtcomplete_prediction.rs"]
mod prediction;
#[path = "completion_replacement.rs"]
mod replacement;
#[path = "vtcomplete_scoring.rs"]
mod scoring;
use crate::completion_match as identifier;
use scoring::{MAX_COMPLETION_WORD_BYTES, MAX_QUERY_BYTES, Scores};
#[cfg(test)]
use scoring::{MAX_REJECTED_BYTES, MAX_REJECTED_WORDS};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum VtCompleteWorkOutcome {
    #[default]
    Exact,
    FuzzyWorkLimited,
}

struct CompletionResult {
    suggestions: Vec<String>,
    details: Vec<super::CompletionDetail>,
    outcome: VtCompleteWorkOutcome,
    deferred_candidates: u32,
}

impl CompletionResult {
    fn limited() -> Self {
        Self {
            suggestions: Vec::new(),
            details: Vec::new(),
            outcome: VtCompleteWorkOutcome::FuzzyWorkLimited,
            deferred_candidates: 1,
        }
    }
}

enum ResolvedPrefix {
    Text(String),
    WorkLimited,
}

// Indexing is optional. Larger documents/lines retain the exact streaming
// behavior, and every retained structure has a separate limit. Four entries
// bound the source snapshots, line metadata, interned words and postings.
const MAX_DOCUMENTS: usize = 4;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_DOCUMENT_LINES: usize = 16_384;
const MAX_WORDS: usize = 4_096;
const MAX_WORD_BYTES: usize = 512 * 1024;
const MAX_POSTINGS: usize = 65_536;
#[cfg(test)]
const MAX_RETAINED_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
#[cfg(test)]
const MAX_RETAINED_SNAPSHOTS: usize = 512;
#[cfg(test)]
const WORD_LIMIT: usize = 50;

#[derive(Clone)]
pub(super) struct VtCompleteSnapshot {
    pub(super) source: crate::completion_source::Source,
    pub(super) rope: ropey::Rope,
    pub(super) document_id: u64,
    pub(super) cursor_line: usize,
    pub(super) cursor_col: usize,
    pub(super) active: bool,
}

pub(super) struct VtCompleteRequest {
    pub(super) active: VtCompleteSnapshot,
    pub(super) edit_baseline: Option<ropey::Rope>,
    pub(super) index: crate::completion_index::CompletionIndex,
    pub(super) explicit: bool,
    pub(super) language: String,
    pub(super) generation: u64,
    pub(super) active_group: usize,
    pub(super) active_tab: usize,
    pub(super) content_version: u64,
    pub(super) cancel: CancellationToken,
    pub(super) sender: BackgroundResultSender,
}

#[derive(Default)]
struct Queue {
    pending: Option<VtCompleteRequest>,
    active_cancel: Option<CancellationToken>,
    running: bool,
    cache: Option<TokenCache>,
    activity: crate::completion_activity::Activity,
}

pub(crate) struct VtCompleteWorker {
    queue: Arc<Mutex<Queue>>,
    activity: crate::completion_activity::Activity,
    #[cfg(test)]
    inactive_snapshots: Weak<Vec<VtCompleteSnapshot>>,
    #[cfg(test)]
    retained_inactive_snapshots: Option<Arc<Vec<VtCompleteSnapshot>>>,
}

impl Default for VtCompleteWorker {
    fn default() -> Self {
        let queue = Queue::default();
        let activity = queue.activity.clone();
        Self {
            queue: Arc::new(Mutex::new(queue)),
            activity,
            #[cfg(test)]
            inactive_snapshots: Weak::default(),
            #[cfg(test)]
            retained_inactive_snapshots: None,
        }
    }
}

impl Drop for VtCompleteWorker {
    fn drop(&mut self) {
        let (active, pending) = {
            let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
            (queue.active_cancel.take(), queue.pending.take())
        };
        if let Some(active) = active {
            active.cancel();
        }
        if let Some(pending) = pending {
            pending.cancel.cancel();
        }
    }
}

impl VtCompleteWorker {
    pub(crate) fn activity(&self) -> &crate::completion_activity::Activity {
        &self.activity
    }
    /// The comparison borrows all unchanged Ropes. Typing only clones the
    /// active Rope and one Arc, rather than every open document plus a Vec.
    #[cfg(test)]
    pub(super) fn snapshots(
        &mut self,
        tabs: &[crate::app::Tab],
        active_tab: usize,
    ) -> (VtCompleteSnapshot, Arc<Vec<VtCompleteSnapshot>>) {
        let active = &tabs[active_tab].editor;
        let eligible = || {
            tabs.iter().enumerate().filter(|(index, tab)| {
                *index != active_tab && !tab.editor.large_file_mode && !tab.editor.bg_loading
            })
        };
        let previous = self.inactive_snapshots.upgrade();
        let unchanged = previous.as_ref().is_some_and(|previous| {
            let mut cached = previous.iter();
            eligible().all(|(_, tab)| {
                cached.next().is_some_and(|cached| {
                    tab.editor.navigation_id == cached.document_id
                        && tab.editor.rope.is_instance(&cached.rope)
                })
            }) && cached.next().is_none()
        });
        let snapshots = if unchanged {
            previous.unwrap()
        } else {
            let snapshots: Arc<Vec<_>> = Arc::new(
                eligible()
                    .map(|(_, tab)| VtCompleteSnapshot {
                        source: Default::default(),
                        rope: tab.editor.rope.clone(),
                        document_id: tab.editor.navigation_id,
                        cursor_line: 0,
                        cursor_col: 0,
                        active: false,
                    })
                    .collect(),
            );
            self.inactive_snapshots = Arc::downgrade(&snapshots);
            // An idle UI cache must not retain an arbitrarily large set of
            // closed documents. Oversized lists are shared only while an
            // active/pending request (or its caller) still owns their Arc.
            self.retained_inactive_snapshots = (snapshots.len() <= MAX_RETAINED_SNAPSHOTS
                && snapshots
                    .iter()
                    .try_fold(0usize, |bytes, snapshot| {
                        bytes.checked_add(snapshot.rope.len_bytes())
                    })
                    .is_some_and(|bytes| bytes <= MAX_RETAINED_SNAPSHOT_BYTES))
            .then(|| Arc::clone(&snapshots));
            snapshots
        };
        (
            VtCompleteSnapshot {
                source: Default::default(),
                rope: active.rope.clone(),
                document_id: active.navigation_id,
                cursor_line: active.cursor_line,
                cursor_col: active.cursor_col,
                active: true,
            },
            snapshots,
        )
    }

    fn push(&self, request: VtCompleteRequest) -> bool {
        let (replaced, active, activity, start) = {
            let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
            let replaced = queue.pending.replace(request);
            let start = !queue.running;
            queue.running = true;
            (
                replaced,
                queue.active_cancel.clone(),
                queue.activity.clone(),
                start,
            )
        };
        // Never drop a replaced snapshot while holding the worker's lock.
        if let Some(replaced) = replaced {
            replaced.cancel.cancel();
        }
        if let Some(active) = active {
            active.cancel();
        }
        activity.wake();
        start
    }

    pub(super) fn submit(&self, request: VtCompleteRequest, pool: BackgroundTaskSender) {
        if self.push(request) {
            let worker_queue = Arc::clone(&self.queue);
            if pool
                .send(Box::new(move || Self::run(worker_queue)))
                .is_err()
            {
                let pending = {
                    let mut queue = self.queue.lock().unwrap_or_else(|error| error.into_inner());
                    queue.running = false;
                    queue.pending.take()
                };
                if let Some(pending) = pending {
                    pending.cancel.cancel();
                }
            }
        }
    }

    fn run(queue: Arc<Mutex<Queue>>) {
        Self::run_with(queue, Self::process);
    }

    fn run_with(
        queue: Arc<Mutex<Queue>>,
        mut process: impl FnMut(
            VtCompleteRequest,
            &mut TokenCache,
            &crate::completion_activity::Activity,
        ),
    ) {
        let _priority = crate::worker_priority::CompletionPriority::new();
        let activity = queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .activity
            .clone();
        let mut cache = queue
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cache
            .take()
            .unwrap_or_default();
        loop {
            let request = {
                let mut queue = queue.lock().unwrap_or_else(|error| error.into_inner());
                match queue.pending.take() {
                    Some(request) => {
                        queue.active_cancel = Some(request.cancel.clone());
                        request
                    }
                    None => {
                        queue.active_cancel = None;
                        queue.cache = Some(cache);
                        queue.running = false;
                        return;
                    }
                }
            };
            if activity.wait(|| request.cancel.is_cancelled()) {
                process(request, &mut cache, &activity);
            }
        }
    }

    fn process(
        request: VtCompleteRequest,
        cache: &mut TokenCache,
        activity: &crate::completion_activity::Activity,
    ) {
        while activity.wait(|| request.cancel.is_cancelled()) {
            let generation = activity.generation();
            if activity.remaining().is_some() {
                continue;
            }
            let cancelled = || request.cancel.is_cancelled() || activity.interrupted(generation);
            let result = Self::compute_request(&request, cache, &cancelled);
            if activity.interrupted(generation) && !request.cancel.is_cancelled() {
                continue; // Resume only after the UI's current interaction settles.
            }
            if let Some(result) = result
                && !request.cancel.is_cancelled()
            {
                let _ = request.sender.send(BackgroundResult::VtComplete {
                    generation: request.generation,
                    active_group: request.active_group,
                    active_tab: request.active_tab,
                    document_id: request.active.document_id,
                    cursor_line: request.active.cursor_line,
                    cursor_col: request.active.cursor_col,
                    content_version: request.content_version,
                    source_identity: request.active.source,
                    source_rope: request.active.rope,
                    language: request.language,
                    suggestions: result.suggestions,
                    details: result.details,
                    work_outcome: result.outcome,
                    deferred_candidates: result.deferred_candidates,
                });
            }
            return;
        }
    }

    fn compute_request(
        request: &VtCompleteRequest,
        cache: &mut TokenCache,
        cancelled: &impl Fn() -> bool,
    ) -> Option<CompletionResult> {
        // No lock is held while scanning, indexing, scoring or publishing.
        let prefix = resolve_prefix_with_minimum(&request.active, 0, cancelled)?;
        let history_before_seed = if request.index.settings().use_history
            && cache.edit_history.missing(request.active.document_id)
            && let Some(baseline) = &request.edit_baseline
        {
            let previous = cache.edit_history.clone();
            if cache
                .edit_history
                .seed(&request.active, baseline, cancelled)
                .is_none()
            {
                cache.edit_history = previous;
                return None;
            }
            Some(previous)
        } else {
            None
        };
        let result = match prefix {
            ResolvedPrefix::Text(prefix) => compute_contextual_invoked(
                &request.active,
                &prefix,
                &request.language,
                &request.index,
                cache,
                request.explicit,
                cancelled,
            ),
            ResolvedPrefix::WorkLimited => Some(CompletionResult::limited()),
        };
        if (result.is_none() || cancelled())
            && let Some(previous) = history_before_seed
        {
            cache.edit_history = previous;
        }
        result.filter(|_| !cancelled())
    }
}

/// Only a bounded suffix is inspected on the input thread. A structural
/// trigger can request useful context suggestions before an identifier starts.
pub(super) fn automatic_trigger(
    editor: &crate::editor::Editor,
    settings: &crate::completion_index::IndexSettings,
) -> bool {
    let Some(prefix) = popup_prefix(editor) else {
        return false;
    };
    if prefix.chars().count() >= settings.min_prefix_chars {
        return true;
    }
    if !settings.context_triggers {
        return false;
    }
    let context = editor_context(editor);
    let suffix = context.trim_end();
    suffix.ends_with('.')
        || suffix.ends_with("::")
        || suffix.ends_with("->")
        || suffix.ends_with('&')
        || suffix.ends_with('*')
        || suffix.ends_with('(')
        || suffix.ends_with(',')
        || suffix.ends_with('=')
        || suffix.ends_with(':')
}

pub(super) fn has_vtcomplete_prefix(editor: &crate::editor::Editor) -> bool {
    let Some(line) = editor.rope.get_line(editor.cursor_line) else {
        return false;
    };
    let col = editor.cursor_col.min(line.len_chars());
    let Some(previous) = col.checked_sub(1).and_then(|col| line.get_char(col)) else {
        return false;
    };
    identifier_char(previous)
}

#[cfg(test)]
fn resolve_prefix(
    snapshot: &VtCompleteSnapshot,
    cancelled: &impl Fn() -> bool,
) -> Option<ResolvedPrefix> {
    resolve_prefix_with_minimum(snapshot, 2, cancelled)
}

fn resolve_prefix_with_minimum(
    snapshot: &VtCompleteSnapshot,
    minimum_bytes: usize,
    cancelled: &impl Fn() -> bool,
) -> Option<ResolvedPrefix> {
    let line = snapshot.rope.get_line(snapshot.cursor_line)?;
    let col = snapshot.cursor_col.min(line.len_chars());
    let mut start = col;
    let mut bytes = 0;
    for (offset, ch) in line.chars_at(col).reversed().enumerate() {
        if offset % 256 == 0 && cancelled() {
            return None;
        }
        if !identifier_char(ch) {
            break;
        }
        bytes += ch.len_utf8();
        if bytes > MAX_QUERY_BYTES {
            return Some(ResolvedPrefix::WorkLimited);
        }
        start -= 1;
    }
    let prefix = line.slice(start..col);
    if prefix.len_bytes() < minimum_bytes || cancelled() {
        return None;
    }
    let mut text = String::with_capacity(prefix.len_bytes());
    for chunk in prefix.chunks() {
        if cancelled() {
            return None;
        }
        text.push_str(chunk);
    }
    Some(ResolvedPrefix::Text(text))
}

/// UI work is bounded independently from worker extraction. A malformed or
/// generated identifier never makes typing traverse its whole logical line.
pub(super) fn popup_prefix(editor: &crate::editor::Editor) -> Option<String> {
    let line = editor.rope.get_line(editor.cursor_line)?;
    let col = editor.cursor_col.min(line.len_chars());
    let mut prefix = Vec::new();
    for ch in line.chars_at(col).reversed() {
        if !identifier_char(ch) {
            break;
        }
        if prefix.len() == 128 {
            return None;
        }
        prefix.push(ch);
    }
    Some(prefix.into_iter().rev().collect())
}

fn identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

pub(super) fn quick_matches(word: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    let lower = prefix.to_lowercase();
    let mut query = lower.chars();
    let mut next = query.next();
    for ch in word.chars().flat_map(char::to_lowercase) {
        if next == Some(ch) {
            next = query.next();
            if next.is_none() {
                return true;
            }
        }
    }
    false
}

pub(super) fn editor_context(editor: &crate::editor::Editor) -> String {
    let snapshot = VtCompleteSnapshot {
        source: Default::default(),
        rope: editor.rope.clone(),
        document_id: editor.navigation_id,
        cursor_line: editor.cursor_line,
        cursor_col: editor.cursor_col,
        active: true,
    };
    context_before_prefix(&snapshot)
}

fn context_before_prefix(snapshot: &VtCompleteSnapshot) -> String {
    let Some(line) = snapshot.rope.get_line(snapshot.cursor_line) else {
        return String::new();
    };
    let col = snapshot.cursor_col.min(line.len_chars());
    let mut backwards = line.chars_at(col).reversed();
    let mut chars = Vec::new();
    let mut prefix = true;
    for ch in backwards.by_ref().take(512) {
        if prefix && identifier_char(ch) {
            continue;
        }
        prefix = false;
        chars.push(ch);
        if chars.len() == 256 {
            break;
        }
    }
    chars.into_iter().rev().collect()
}

#[cfg(test)]
fn quiet_context(snapshot: &VtCompleteSnapshot) -> bool {
    let Some(ResolvedPrefix::Text(prefix)) = resolve_prefix_with_minimum(snapshot, 0, &|| false)
    else {
        return false;
    };
    prediction::Cache::default()
        .analyze(snapshot, &prefix, &|| false)
        .is_some_and(|analysis| analysis.context.domain != crate::completion_context::Domain::Code)
}

enum WordToken<'a> {
    Word(&'a str),
    Oversized,
}

/// Bounded extraction never allocates a giant completion label. The callback
/// observes oversize omissions separately from true nonmatches.
fn visit_words(
    line: ropey::RopeSlice<'_>,
    scores: &mut Scores<'_>,
    word: &mut String,
    cancelled: &impl Fn() -> bool,
    mut visit: impl FnMut(&mut Scores<'_>, WordToken<'_>) -> Option<()>,
) -> Option<bool> {
    // Reuse one bounded word buffer across every line of this request.
    // Clear first as cancellation may have left a partial token behind.
    word.clear();
    let mut oversized = false;
    let finish =
        |word: &mut String,
         oversized: &mut bool,
         scores: &mut Scores<'_>,
         visit: &mut dyn FnMut(&mut Scores<'_>, WordToken<'_>) -> Option<()>| {
            if *oversized {
                scores.defer_candidate();
                visit(scores, WordToken::Oversized)?;
            } else if word.len() >= 2 {
                visit(scores, WordToken::Word(word))?;
            }
            word.clear();
            *oversized = false;
            Some(())
        };
    for chunk in line.chunks() {
        if cancelled() {
            return None;
        }
        if !scores.charge_scan_bytes(chunk.len()) {
            return Some(false);
        }
        for ch in chunk.chars() {
            if identifier_char(ch) {
                if !oversized {
                    if ch.len_utf8() > MAX_COMPLETION_WORD_BYTES.saturating_sub(word.len()) {
                        oversized = true;
                        word.clear();
                    } else {
                        word.push(ch);
                    }
                }
            } else {
                finish(word, &mut oversized, scores, &mut visit)?;
            }
        }
    }
    finish(word, &mut oversized, scores, &mut visit)?;
    if cancelled() { None } else { Some(true) }
}

struct DocumentIndex {
    document_id: u64,
    rope: ropey::Rope,
    lines: Vec<LineIndex>,
    words: Vec<Arc<str>>,
    word_ids: FxHashMap<Arc<str>, u32>,
    word_bytes: usize,
    postings: usize,
    last_used: u64,
    context_lines: FxHashMap<usize, (usize, Vec<LocalObservation>)>,
    context_bytes: usize,
}

enum LineIndex {
    Missing,
    Cached(Vec<u32>),
    // A line that exceeds a word/posting budget is scanned once per request.
    // Retrying index construction before every scan would duplicate that work.
    Streaming,
}

struct SeenWords([u64; MAX_WORDS / 64]);

impl Default for SeenWords {
    fn default() -> Self {
        Self([0; MAX_WORDS / 64])
    }
}

impl SeenWords {
    fn insert(&mut self, id: u32) -> bool {
        let word = &mut self.0[id as usize / 64];
        let bit = 1u64 << (id as usize % 64);
        let fresh = *word & bit == 0;
        *word |= bit;
        fresh
    }
}

impl DocumentIndex {
    fn new(snapshot: &VtCompleteSnapshot, last_used: u64) -> Self {
        Self {
            document_id: snapshot.document_id,
            rope: snapshot.rope.clone(),
            lines: std::iter::repeat_with(|| LineIndex::Missing)
                .take(snapshot.rope.len_lines())
                .collect(),
            words: Vec::new(),
            word_ids: FxHashMap::default(),
            word_bytes: 0,
            postings: 0,
            last_used,
            context_lines: FxHashMap::default(),
            context_bytes: 0,
        }
    }

    fn update(&mut self, rope: &ropey::Rope, cancelled: &impl Fn() -> bool) -> Option<()> {
        if self.rope.is_instance(rope) {
            return Some(());
        }
        let (old, new) = changed_lines(&self.rope, rope, cancelled)?;
        let shift = new.len() as isize - old.len() as isize;
        let mut contexts = FxHashMap::default();
        self.context_bytes = 0;
        for (line, (bytes, observed)) in self.context_lines.drain() {
            if old.contains(&line) {
                continue;
            }
            let line = if line >= old.end {
                line.saturating_add_signed(shift)
            } else {
                line
            };
            self.context_bytes += bytes;
            contexts.insert(line, (bytes, observed));
        }
        self.context_lines = contexts;
        if old.len() == new.len() {
            for line in &mut self.lines[old] {
                if let LineIndex::Cached(postings) = std::mem::replace(line, LineIndex::Missing) {
                    self.postings -= postings.len();
                }
            }
        } else {
            for line in self.lines.splice(
                old,
                std::iter::repeat_with(|| LineIndex::Missing).take(new.len()),
            ) {
                if let LineIndex::Cached(postings) = line {
                    self.postings -= postings.len();
                }
            }
        }
        self.rope = rope.clone();
        debug_assert_eq!(self.lines.len(), rope.len_lines());
        Some(())
    }

    fn line_context(&mut self, line: usize) -> Option<&[LocalObservation]> {
        if !self.context_lines.contains_key(&line) {
            let source = self.rope.line(line);
            let bytes = source.len_bytes();
            if bytes > 4096
                || self.context_bytes + bytes > 64 * 1024
                || self.context_lines.len() >= 256
            {
                return None;
            }
            let observed = local_observations(&source.to_string());
            self.context_bytes += bytes;
            self.context_lines.insert(line, (bytes, observed));
        }
        self.context_lines
            .get(&line)
            .map(|(_, observed)| observed.as_slice())
    }

    fn prune_context(&mut self, cursor: usize) {
        self.context_bytes = 0;
        self.context_lines.retain(|&line, (bytes, _)| {
            if cursor.abs_diff(line) >= 128 {
                false
            } else {
                self.context_bytes += *bytes;
                true
            }
        });
    }

    fn scan_line(
        &mut self,
        rope: &ropey::Rope,
        line: usize,
        proximity: usize,
        scores: &mut Scores<'_>,
        seen: &mut SeenWords,
        word: &mut String,
        cancelled: &impl Fn() -> bool,
    ) -> Option<bool> {
        if cancelled() {
            return None;
        }
        if matches!(self.lines[line], LineIndex::Missing) {
            let mut postings = Vec::new();
            let mut line_seen = SeenWords::default();
            let mut cacheable = self.postings < MAX_POSTINGS;
            let complete =
                visit_words(rope.line(line), scores, word, cancelled, |scores, token| {
                    let WordToken::Word(word) = token else {
                        cacheable = false;
                        return Some(());
                    };
                    let id = if let Some(&id) = self.word_ids.get(word) {
                        Some(id)
                    } else if cacheable
                        && self.words.len() < MAX_WORDS
                        && word.len() <= MAX_WORD_BYTES.saturating_sub(self.word_bytes)
                    {
                        let id = self.words.len() as u32;
                        let word: Arc<str> = word.into();
                        self.word_bytes += word.len();
                        self.words.push(Arc::clone(&word));
                        self.word_ids.insert(word, id);
                        Some(id)
                    } else {
                        cacheable = false;
                        None
                    };
                    if let Some(id) = id {
                        if seen.insert(id) {
                            scores.add(word, proximity, false, cancelled)?;
                        }
                        if cacheable && line_seen.insert(id) {
                            if postings.len() >= MAX_POSTINGS - self.postings {
                                cacheable = false;
                            } else {
                                postings.push(id);
                            }
                        }
                    } else {
                        scores.add(word, proximity, false, cancelled)?;
                    }
                    Some(())
                })?;
            if complete {
                if cacheable {
                    self.postings += postings.len();
                    self.lines[line] = LineIndex::Cached(postings);
                } else {
                    self.lines[line] = LineIndex::Streaming;
                }
            }
            return Some(complete);
        }
        if let LineIndex::Cached(postings) = &self.lines[line] {
            for (index, &word) in postings.iter().enumerate() {
                if index % 256 == 0 && cancelled() {
                    return None;
                }
                if seen.insert(word) {
                    scores.add(&self.words[word as usize], proximity, false, cancelled)?;
                }
            }
            Some(true)
        } else {
            visit_words(rope.line(line), scores, word, cancelled, |scores, token| {
                if let WordToken::Word(word) = token
                    && self.word_ids.get(word).is_none_or(|&id| seen.insert(id))
                {
                    scores.add(word, proximity, false, cancelled)?;
                }
                Some(())
            })
        }
    }
}

#[derive(Default)]
struct TokenCache {
    documents: Vec<DocumentIndex>,
    prediction: prediction::Cache,
    edit_history: edit_history::Cache,
    use_counter: u64,
    #[cfg(test)]
    builds: usize,
    #[cfg(test)]
    reference_admission: bool,
}

#[derive(Clone, Copy, Default)]
struct CompletionEvidence {
    generated: Option<patterns::Kind>,
    context_score: i64,
    frequency: u32,
    calls: u32,
    local: bool,
    semantic_score: i64,
    semantic_seen: bool,
    lexical_seen: bool,
    matched_calls: u32,
    matched_uses: u32,
    local_matched_calls: u32,
    local_matched_uses: u32,
    code_uses: u32,
    code_calls: u32,
    project_receiver: bool,
    project_callable: bool,
    domain: crate::completion_context::Domain,
    declaration: bool,
    type_position: bool,
    history_score: i64,
    edit_history: bool,
    local_context_score: i64,
    example_seen: bool,
    example_probability: f64,
    example_callable: Option<bool>,
    example_reference: bool,
    file_frequency: u32,
}

#[derive(Clone, Default)]
struct ContextKey {
    previous: String,
    before_previous: String,
    punctuation: String,
}

struct LocalObservation {
    word: String,
    domain: crate::completion_context::Domain,
    context: ContextKey,
    callable: bool,
}

fn local_observations(text: &str) -> Vec<LocalObservation> {
    use crate::completion_context::{Analyzer, Event};
    let mut analyzer = Analyzer::default();
    let mut observed: Vec<LocalObservation> = Vec::new();
    let mut positions = text.char_indices().enumerate().peekable();
    let mut last = None;
    let mut record = |event: Event<'_>| match event {
        Event::Word {
            word,
            context,
            offset,
        } => {
            while positions.peek().is_some_and(|&(at, _)| at < offset) {
                positions.next();
            }
            let byte = positions.peek().map_or(text.len(), |&(_, (byte, _))| byte);
            last = Some((offset, observed.len()));
            observed.push(LocalObservation {
                word: word.to_owned(),
                context: ContextKey::new(&text[..byte]),
                callable: false,
                domain: context.domain,
            });
        }
        Event::Call { offset } => {
            if let Some((at, index)) = last
                && at == offset
            {
                observed[index].callable = true;
            }
        }
    };
    analyzer.feed(text, &mut record);
    analyzer.flush(record);
    observed
}

impl ContextKey {
    fn new(context: &str) -> Self {
        let context = context.trim_end();
        let punctuation: String = context
            .chars()
            .rev()
            .take_while(|ch| !identifier_char(*ch) && !ch.is_whitespace())
            .take(4)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let mut words = context
            .split(|ch: char| !identifier_char(ch))
            .filter(|word| !word.is_empty())
            .rev();
        Self {
            punctuation,
            previous: words.next().unwrap_or("").to_owned(),
            before_previous: words.next().unwrap_or("").to_owned(),
        }
    }

    fn relevance(&self, other: &Self) -> i64 {
        if !self.punctuation.is_empty()
            && self.punctuation == other.punctuation
            && self.previous != other.previous
        {
            return if self.punctuation.ends_with('&') {
                100
            } else {
                24
            };
        }
        if self.previous.is_empty() || self.previous != other.previous {
            return 0;
        }
        let mut score = 36;
        if self.punctuation == other.punctuation {
            score += if matches!(self.punctuation.as_str(), "." | "::" | "->" | "?.") {
                160
            } else {
                56
            };
        }
        if !self.before_previous.is_empty() && self.before_previous == other.before_previous {
            score += 36;
        }
        score
    }
}

/// Requests inspect a small neighborhood for fresh, unsaved identifiers and
/// use the project vocabulary for everything else. This bounds work regardless
/// of project size, open tab count, and the number of nonmatching source lines.
#[cfg(test)]
fn compute_contextual(
    active: &VtCompleteSnapshot,
    prefix: &str,
    language: &str,
    index: &crate::completion_index::CompletionIndex,
    cache: &mut TokenCache,
    cancelled: impl Fn() -> bool,
) -> Option<CompletionResult> {
    compute_contextual_invoked(active, prefix, language, index, cache, true, cancelled)
}

fn compute_contextual_invoked(
    active: &VtCompleteSnapshot,
    prefix: &str,
    language: &str,
    index: &crate::completion_index::CompletionIndex,
    cache: &mut TokenCache,
    explicit: bool,
    cancelled: impl Fn() -> bool,
) -> Option<CompletionResult> {
    debug_assert!(active.active);
    cache.begin_request();
    let limits = crate::fuzzy_score::FuzzyWorkLimits {
        request_bytes: 384 * 1024,
        request_cells: 1_048_576,
        ..scoring::work_limits()
    };
    let mut budget = crate::fuzzy_score::FuzzyWorkBudget::new(limits);
    let lower = match scoring::normalize_query(prefix, &mut budget, &cancelled)? {
        Ok(lower) => lower,
        Err(()) => return Some(CompletionResult::limited()),
    };
    let context = context_before_prefix(active);
    let key = ContextKey::new(&context);
    // Canceled requests cannot publish staged temporal evidence. These clones
    // share bounded immutable Rope leaves rather than copying source strings.
    let use_edit_history = index.settings().use_history;
    if !use_edit_history {
        cache.edit_history = edit_history::Cache::default();
    }
    let mut edit_history = cache.edit_history.clone();
    if use_edit_history {
        edit_history.observe(active, prefix, &cancelled)?;
    }
    let mut scores = Scores::new(prefix, &lower, budget);
    scores.contextual = true;
    let mut evidence = FxHashMap::<String, CompletionEvidence>::default();
    let mut analysis = cache.prediction.analyze(active, prefix, &cancelled)?;
    let mut source = active.source;
    if source.kind.family == 0 {
        source.kind = crate::completion_source::Kind::new(None, language);
    }
    analysis.context.source_family = source.kind.family;
    analysis.context.source_scope = source.file;
    if !explicit && analysis.context.domain != crate::completion_context::Domain::Code {
        return Some(CompletionResult {
            suggestions: Vec::new(),
            details: Vec::new(),
            outcome: VtCompleteWorkOutcome::Exact,
            deferred_candidates: 0,
        });
    }
    let mut built = false;
    #[cfg(test)]
    let reference_admission = cache.reference_admission;
    #[cfg(not(test))]
    let reference_admission = false;
    let prepared = prepare_documents(
        &mut cache.documents,
        cache.use_counter,
        active,
        &cancelled,
        reference_admission,
        &mut built,
    )?;
    #[cfg(test)]
    {
        cache.builds += usize::from(built);
    }
    let history = index.history_scores(prefix, analysis.context, explicit);
    // Occurrences establish relevance. A source identity is used for history
    // and to ground a cached copy of this buffer, never to promote file groups.
    let (example_left, example_right, file_before, file_after) = example_context(active, prefix);
    let example_query =
        crate::completion_index::examples::Query::new(&example_left, &example_right);
    let mut learned_examples = crate::completion_index::examples::Evidence::default();
    // Recent text gets first use of the verification allowance. Indexed text
    // cannot starve a freshly typed local relationship with generated copies.
    learned_examples.collect(&example_query, &example_left, true);
    learned_examples.collect(&example_query, &file_before, false);
    learned_examples.collect(&example_query, &file_after, false);
    let mut generated = if analysis.context.domain == crate::completion_context::Domain::Code {
        let start = active.rope.line_to_char(active.cursor_line)
            + active.cursor_col.saturating_sub(prefix.chars().count());
        patterns::predict(
            &example_left,
            &example_right,
            prefix,
            start <= 1024,
            &cancelled,
        )?
    } else {
        patterns::Predictions::default()
    };
    if generated.limited {
        scores.defer_candidate();
    }
    // Admit verified current-buffer predictions before unrelated candidates can
    // consume the fuzzy allowance. Ranking still uses the same evidence model.
    for word in generated.novel_candidates() {
        scores.add(word, 0, false, &cancelled)?;
    }
    index.learn_examples(&example_query, source.file, &mut learned_examples);
    if learned_examples.limited {
        scores.defer_candidate();
    }
    for learned in index.query_for_invocation_cancellable(
        prefix,
        &context,
        analysis.context,
        &history,
        explicit,
        &cancelled,
    ) {
        if cancelled() {
            return None;
        }
        scores.add(&learned.word, 1000, false, &cancelled)?;
        if scores.matches.contains_key(&learned.word) {
            evidence.insert(
                learned.word,
                CompletionEvidence {
                    context_score: learned.context_score,
                    frequency: learned.frequency,
                    calls: if learned.callable {
                        learned.frequency.max(2)
                    } else {
                        0
                    },
                    local: false,
                    domain: learned.domain,
                    lexical_seen: true,
                    semantic_score: if learned.domain == analysis.context.domain {
                        0
                    } else if learned.domain == crate::completion_context::Domain::Comment {
                        -145
                    } else {
                        -65
                    },
                    semantic_seen: true,
                    project_receiver: learned.receiver_match,
                    project_callable: learned.callable,
                    matched_uses: learned.context_uses,
                    matched_calls: learned.context_calls,
                    history_score: learned.history_score,
                    local_context_score: 0,
                    ..CompletionEvidence::default()
                },
            );
        }
    }
    if let Some(prepared) = prepared {
        cache.documents[prepared].prune_context(active.cursor_line);
    }
    let mut seen = SeenWords::default();
    let mut word = String::new();
    let mut context_bytes = 0usize;
    'nearby: for distance in 0..128 {
        for line in [
            active.cursor_line.checked_sub(distance),
            (distance > 0).then(|| active.cursor_line.saturating_add(distance)),
        ]
        .into_iter()
        .flatten()
        {
            if line >= active.rope.len_lines() {
                continue;
            }
            let complete = if let Some(prepared) = prepared {
                cache.documents[prepared].scan_line(
                    &active.rope,
                    line,
                    distance,
                    &mut scores,
                    &mut seen,
                    &mut word,
                    &cancelled,
                )?
            } else {
                visit_words(
                    active.rope.line(line),
                    &mut scores,
                    &mut word,
                    &cancelled,
                    |scores, token| {
                        if let WordToken::Word(word) = token {
                            scores.add(word, distance, false, &cancelled)?;
                        }
                        Some(())
                    },
                )?
            };
            if !complete {
                break 'nearby;
            }
            // Context extraction has its own small cap. Oversized/minified
            // lines still use the streaming word scorer above, without copies.
            let source = active.rope.line(line);
            if source.len_bytes() <= 4096 && context_bytes + source.len_bytes() <= 48 * 1024 {
                context_bytes += source.len_bytes();
                let fallback;
                let observed = if let Some(prepared) = prepared {
                    cache.documents[prepared]
                        .line_context(line)
                        .unwrap_or_default()
                } else {
                    fallback = local_observations(&source.to_string());
                    &fallback
                };
                for observed in observed {
                    if !scores.matches.contains_key(&observed.word) {
                        continue;
                    }
                    let item = evidence.entry(observed.word.clone()).or_default();
                    item.local = true;
                    item.local_context_score = 170;
                    if observed.domain == analysis.context.domain {
                        item.frequency = item.frequency.saturating_add(1);
                        item.context_score =
                            item.context_score.max(key.relevance(&observed.context));
                    }
                    if !item.lexical_seen
                        || observed.domain == crate::completion_context::Domain::Code
                            && !item.semantic_seen
                    {
                        item.domain = observed.domain;
                        item.semantic_score = if observed.domain == analysis.context.domain {
                            0
                        } else {
                            -145
                        };
                        item.lexical_seen = true;
                    }
                    if observed.callable {
                        item.calls = item.calls.saturating_add(1);
                    }
                }
            }
        }
    }
    for observed in analysis.prior.iter().chain(analysis.current) {
        if !scores.matches.contains_key(&observed.word) {
            continue;
        }
        let item = evidence.entry(observed.word.clone()).or_default();
        if observed.context.domain == crate::completion_context::Domain::Code {
            item.code_uses = item.code_uses.saturating_add(1);
            item.code_calls = item.code_calls.saturating_add(u32::from(observed.callable));
        }
        if live_file_prior_enabled() && observed.context.domain == analysis.context.domain {
            // The live buffer supplies a domain-conditioned file prior even
            // before its index snapshot is published. Code frequency must not
            // displace literal words during explicit completion inside quotes.
            item.file_frequency = item.file_frequency.saturating_add(1);
        }
        let affinity = analysis.context.affinity(observed.context, true);
        if !item.semantic_seen || affinity > item.semantic_score {
            item.semantic_score = affinity;
            item.semantic_seen = true;
            item.domain = observed.context.domain;
            item.declaration = observed.context.declaration;
            item.type_position = observed.context.type_position;
        }
        let structural_match = if analysis.context.member() {
            analysis.context.keys[..2]
                .iter()
                .zip(&observed.context.keys[..2])
                .any(|(&a, &b)| a != 0 && a == b)
        } else if analysis.context.keys[4] != 0 {
            analysis.context.keys[4] == observed.context.keys[4]
        } else {
            analysis.context.keys[2] != 0 && analysis.context.keys[2] == observed.context.keys[2]
        };
        if structural_match && observed.context.domain == crate::completion_context::Domain::Code {
            item.local_matched_uses = item.local_matched_uses.saturating_add(1);
            item.local_matched_calls = item
                .local_matched_calls
                .saturating_add(u32::from(observed.callable));
            item.matched_uses = item.matched_uses.saturating_add(1);
            item.matched_calls = item
                .matched_calls
                .saturating_add(u32::from(observed.callable));
        }
    }
    for (word, boost) in history {
        if scores.matches.contains_key(&word) {
            evidence.entry(word).or_default().history_score = boost;
        }
    }
    for (word, frequency, calls) in index.current_vocabulary(source.file, prefix) {
        scores.add(&word, 0, false, &cancelled)?;
        let item = evidence.entry(word).or_default();
        item.file_frequency = item.file_frequency.max(frequency);
        item.local = true;
        // Literal current-file usage protects a field/callback from unrelated
        // programs that happen to call the same spelling.
        item.code_uses = frequency;
        item.code_calls = calls;
    }
    for word in learned_examples.candidates() {
        scores.add(word, 0, false, &cancelled)?;
        let item = evidence.entry(word.to_owned()).or_default();
        item.example_probability = learned_examples.probability(word, 0.);
        item.example_seen = item.example_probability >= 0.25;
        item.example_callable = learned_examples.callable(word);
        item.example_reference = learned_examples
            .call_probability(word)
            .is_some_and(|p| p < 0.5);
    }
    generated.ground_relations(|word| scores.matches.contains_key(word));
    for word in generated.candidates() {
        if scores.matches.contains_key(word) {
            let item = evidence.entry(word.to_owned()).or_default();
            item.generated = generated.kind(word);
            item.local = true;
            item.example_probability = generated.probability(word, item.example_probability);
        }
    }
    if use_edit_history && analysis.context.domain == crate::completion_context::Domain::Code {
        for word in edit_history.predict(active, prefix) {
            scores.add(&word, 0, false, &cancelled)?;
            if scores.matches.contains_key(&word) {
                let item = evidence.entry(word).or_default();
                item.edit_history = true;
                item.local = true;
            }
        }
    }
    // Empty-prefix structural requests should suggest identifiers and learned
    // patterns, rather than filling the list with every language keyword.
    if !prefix.is_empty() && !analysis.context.member() {
        for &keyword in language_keywords(language) {
            scores.add(keyword, 500, true, &cancelled)?;
        }
    }
    if !index.settings().complete_calls
        || matches!(
            key.previous.as_str(),
            "fn" | "def"
                | "func"
                | "function"
                | "class"
                | "struct"
                | "enum"
                | "trait"
                | "interface"
                | "type"
                | "let"
                | "var"
                | "const"
        )
    {
        for value in evidence.values_mut() {
            value.calls = 0;
            value.matched_calls = 0;
            value.local_matched_calls = 0;
            value.code_calls = 0;
            value.project_callable = false;
            value.example_callable = Some(false);
        }
    }
    if cancelled() {
        None
    } else {
        let mut result = scores.finish_contextual(
            evidence,
            prefix,
            language,
            analysis.context,
            explicit,
            &learned_examples,
            &generated,
        );
        if analysis.context.domain == crate::completion_context::Domain::Code
            && let Some(number) =
                numbers::predict(&example_left, &example_right, prefix, &cancelled)?
        {
            if let Some(existing) = result
                .suggestions
                .iter()
                .position(|word| *word == number.word)
            {
                result.suggestions.remove(existing);
                result.details.remove(existing);
            }
            result.suggestions.insert(0, number.word);
            result.details.insert(
                0,
                super::CompletionDetail {
                    kind: super::CompletionKind::Value,
                    annotation: if number.sequence {
                        "number sequence"
                    } else {
                        "number"
                    },
                    context: analysis.context,
                },
            );
            result.suggestions.truncate(10);
            result.details.truncate(10);
        }
        if cancelled() {
            return None;
        }
        cache.edit_history = edit_history;
        replacement::refine(&mut result, active, prefix, explicit);
        Some(result)
    }
}

fn live_file_prior_enabled() -> bool {
    #[cfg(test)]
    {
        thread_local! {
            static ENABLED: bool = std::env::var_os("VTCODE_DISABLE_LIVE_FILE_PRIOR").is_none();
        }
        return ENABLED.with(|enabled| *enabled);
    }
    #[cfg(not(test))]
    true
}

/// Visible bounded context only. Skip the complete identifier at the caret in
/// the right context, including its untyped tail during replacement edits.
fn example_context(
    active: &VtCompleteSnapshot,
    prefix: &str,
) -> (String, String, String, String) {
    let cursor = active.rope.line_to_char(active.cursor_line) + active.cursor_col;
    let start = cursor.saturating_sub(prefix.chars().count());
    let recent_start = start.saturating_sub(1024);
    let left = active.rope.slice(recent_start..start).to_string();
    let mut end = cursor;
    let limit = (cursor + 256).min(active.rope.len_chars());
    while end < limit && identifier_char(active.rope.char(end)) {
        end += 1;
    }
    let right = if end < active.rope.len_chars() && identifier_char(active.rope.char(end)) {
        String::new()
    } else {
        let line_end = active
            .rope
            .line_to_char((active.cursor_line + 1).min(active.rope.len_lines()));
        active
            .rope
            .slice(end..(end + 128).min(line_end))
            .to_string()
    };
    let before = active
        .rope
        .slice(start.saturating_sub(3072)..recent_start)
        .to_string();
    let after_start = active
        .rope
        .line_to_char((active.cursor_line + 1).min(active.rope.len_lines()));
    let after = active
        .rope
        .slice(after_start..(after_start + 2048).min(active.rope.len_chars()))
        .to_string();
    (left, right, before, after)
}

impl TokenCache {
    fn begin_request(&mut self) {
        self.use_counter = self.use_counter.wrapping_add(1);
        if self.use_counter == 0 {
            for document in &mut self.documents {
                document.last_used = 0;
            }
            self.use_counter = 1;
        }
    }

    #[cfg(test)]
    fn prepare(
        &mut self,
        snapshot: &VtCompleteSnapshot,
        cancelled: &impl Fn() -> bool,
    ) -> Option<Option<usize>> {
        #[cfg(test)]
        if self.reference_admission {
            self.begin_request();
        }
        let mut built = false;
        let index = prepare_documents(
            &mut self.documents,
            self.use_counter,
            snapshot,
            cancelled,
            self.reference_admission,
            &mut built,
        )?;
        #[cfg(test)]
        {
            self.builds += usize::from(built);
        }
        Some(index)
    }
}

fn prepare_documents(
    documents: &mut Vec<DocumentIndex>,
    use_counter: u64,
    snapshot: &VtCompleteSnapshot,
    cancelled: &impl Fn() -> bool,
    reference_admission: bool,
    built: &mut bool,
) -> Option<Option<usize>> {
    if cancelled() {
        return None;
    }
    if snapshot.rope.len_bytes() > MAX_DOCUMENT_BYTES
        || snapshot.rope.len_lines() > MAX_DOCUMENT_LINES
    {
        // Drop any now-ineligible old snapshot on the worker, not the UI.
        documents.retain(|doc| doc.document_id != snapshot.document_id);
        return Some(None);
    }
    if let Some(index) = documents
        .iter()
        .position(|doc| doc.document_id == snapshot.document_id)
    {
        let document = &mut documents[index];
        document.update(&snapshot.rope, cancelled)?;
        document.last_used = use_counter;
        return Some(Some(index));
    }
    if documents.len() == MAX_DOCUMENTS {
        let oldest = documents
            .iter()
            .enumerate()
            .min_by_key(|(_, doc)| doc.last_used)
            .unwrap()
            .0;
        // Keep the active document and the other indexes already used
        // by this request. A traversal of five or more tabs must not
        // evict every index and rebuild them all on the next keystroke.
        // Remaining documents still supply suggestions by streaming.
        let used_this_request = documents[oldest].last_used == use_counter;
        let used_this_request = used_this_request && !reference_admission;
        if used_this_request {
            return Some(None);
        }
        documents.swap_remove(oldest);
    }
    *built = true;
    documents.push(DocumentIndex::new(snapshot, use_counter));
    Some(Some(documents.len() - 1))
}

#[cfg(test)]
fn compute<'a>(
    snapshots: impl Clone + Iterator<Item = &'a VtCompleteSnapshot>,
    prefix: &str,
    language: &str,
    cache: &mut TokenCache,
    cancelled: impl Fn() -> bool,
) -> Option<CompletionResult> {
    compute_with_limits(
        snapshots,
        prefix,
        language,
        cache,
        scoring::work_limits(),
        cancelled,
    )
}

#[cfg(test)]
fn compute_with_limits<'a>(
    snapshots: impl Clone + Iterator<Item = &'a VtCompleteSnapshot>,
    prefix: &str,
    language: &str,
    cache: &mut TokenCache,
    limits: crate::fuzzy_score::FuzzyWorkLimits,
    cancelled: impl Fn() -> bool,
) -> Option<CompletionResult> {
    cache.begin_request();
    let mut budget = crate::fuzzy_score::FuzzyWorkBudget::new(limits);
    let lower = match scoring::normalize_query(prefix, &mut budget, &cancelled)? {
        Ok(lower) => lower,
        Err(()) => return Some(CompletionResult::limited()),
    };
    let mut scores = Scores::new(prefix, &lower, budget);
    let mut word = String::new();
    'documents: for snapshot in snapshots
        .clone()
        .filter(|snapshot| snapshot.active)
        .chain(snapshots.filter(|snapshot| !snapshot.active))
    {
        let index = cache.prepare(snapshot, &cancelled)?;
        let mut seen = SeenWords::default();
        let mut word_count = scores.matches.len();
        let mut scan = |line, proximity| {
            #[cfg(test)]
            if cache.reference_admission {
                word = String::new();
            }
            let complete = if let Some(index) = index {
                cache.documents[index].scan_line(
                    &snapshot.rope,
                    line,
                    proximity,
                    &mut scores,
                    &mut seen,
                    &mut word,
                    &cancelled,
                )
            } else {
                visit_words(
                    snapshot.rope.line(line),
                    &mut scores,
                    &mut word,
                    &cancelled,
                    |scores, token| {
                        if let WordToken::Word(word) = token {
                            scores.add(word, proximity, false, &cancelled)?;
                        }
                        Some(())
                    },
                )
            }?;
            Some((scores.matches.len(), complete))
        };
        if snapshot.active {
            for distance in 0..snapshot.rope.len_lines() {
                if let Some(line) = snapshot.cursor_line.checked_sub(distance) {
                    let progress = scan(line, distance)?;
                    word_count = progress.0;
                    if !progress.1 {
                        break 'documents;
                    }
                }
                if distance > 0
                    && snapshot.cursor_line.saturating_add(distance) < snapshot.rope.len_lines()
                {
                    let progress = scan(snapshot.cursor_line + distance, distance)?;
                    word_count = progress.0;
                    if !progress.1 {
                        break 'documents;
                    }
                }
                if word_count >= WORD_LIMIT {
                    break;
                }
            }
        } else {
            for line in 0..snapshot.rope.len_lines() {
                let progress = scan(line, 1000 + line)?;
                word_count = progress.0;
                if !progress.1 {
                    break 'documents;
                }
                if word_count >= WORD_LIMIT {
                    break;
                }
            }
            if word_count >= WORD_LIMIT {
                break;
            }
        }
    }
    if cancelled() {
        return None;
    }
    // A reserved keyword budget is merged after document traversal, retaining
    // the ordinary fifty-word gate while still helping a work-limited request.
    for &keyword in language_keywords(language) {
        scores.add(keyword, 0, true, &cancelled)?;
    }
    if cancelled() {
        None
    } else {
        Some(scores.finish())
    }
}

#[cfg(test)]
pub(super) fn compute_vtcomplete_suggestions(
    snapshots: Vec<VtCompleteSnapshot>,
    prefix: &str,
    language: &str,
    cancel: &CancellationToken,
) -> Option<Vec<String>> {
    compute(
        snapshots.iter(),
        prefix,
        language,
        &mut TokenCache::default(),
        || cancel.is_cancelled(),
    )
    .map(|result| result.suggestions)
}

#[cfg(test)]
#[path = "vtcomplete_numeric_tests.rs"]
mod numeric_tests;
#[cfg(test)]
#[path = "vtcomplete_quality_tests.rs"]
mod quality_tests;
#[cfg(test)]
#[path = "vtcomplete_source_tests.rs"]
mod source_tests;
#[cfg(test)]
#[path = "vtcomplete_worker_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "vtcomplete_generation_replay_tests.rs"]
mod generation_replay_tests;
#[cfg(test)]
#[path = "vtcomplete_learning_replay_tests.rs"]
mod learning_replay_tests;
#[cfg(test)]
#[path = "vtcomplete_transform_replay_tests.rs"]
mod transform_replay_tests;
#[cfg(test)]
#[path = "vtcomplete_vtcode_capacity_tests.rs"]
mod vtcode_capacity_tests;
