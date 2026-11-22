use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;

use crate::background::{
    BackgroundResult, BackgroundResultSender, BackgroundTaskSender, CancellationToken,
};

use crate::app_vtcomplete_settings::Preferences;
use crate::completion_examples;
use crate::completion_history::History;

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

// Do not retain large editor snapshots for contextual and edit history.
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_DOCUMENT_LINES: usize = 16_384;

#[derive(Clone)]
pub(super) struct VtCompleteSnapshot {
    pub(super) source: crate::completion_source::Source,
    pub(super) rope: ropey::Rope,
    pub(super) document_id: u64,
    pub(super) cursor_line: usize,
    pub(super) cursor_col: usize,
}

#[derive(Default)]
pub(super) struct PopupOptions {
    pub(super) preferences: Preferences,
    pub(super) history: Option<Arc<History>>,
}

pub(super) struct VtCompleteRequest {
    pub(super) active: VtCompleteSnapshot,
    pub(super) edit_baseline: Option<ropey::Rope>,
    pub(super) options: PopupOptions,
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
    cache: Option<CompletionCache>,
    activity: crate::completion_activity::Activity,
}

pub(crate) struct VtCompleteWorker {
    queue: Arc<Mutex<Queue>>,
    activity: crate::completion_activity::Activity,
}

impl Default for VtCompleteWorker {
    fn default() -> Self {
        let queue = Queue::default();
        let activity = queue.activity.clone();
        Self {
            queue: Arc::new(Mutex::new(queue)),
            activity,
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
            &mut CompletionCache,
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
        cache: &mut CompletionCache,
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
        cache: &mut CompletionCache,
        cancelled: &impl Fn() -> bool,
    ) -> Option<CompletionResult> {
        cache.use_document(request.active.document_id);
        let prefix = resolve_prefix_with_minimum(&request.active, 0, cancelled)?;
        let history_before_seed = if request.options.preferences.use_history
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
                &request.options,
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
pub(super) fn automatic_trigger(editor: &crate::editor::Editor, settings: &Preferences) -> bool {
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

/// [`identifier_char`] for an ASCII byte.
fn ascii_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
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
        .is_some_and(|analysis| {
            analysis.context.domain == crate::completion_context::Domain::String
        })
}

/// Score words directly from a buffer line. Oversized identifiers consume
/// bounded scratch space, and every chunk counts against the request budget.
fn scan_words(
    line: ropey::RopeSlice<'_>,
    proximity: usize,
    scores: &mut Scores<'_>,
    word: &mut String,
    cancelled: &impl Fn() -> bool,
) -> Option<bool> {
    word.clear();
    let mut oversized = false;
    for chunk in line.chunks() {
        if cancelled() {
            return None;
        }
        if !scores.charge_scan_bytes(chunk.len()) {
            return Some(false);
        }
        if chunk.is_ascii() {
            // Source text is nearly always ASCII: append whole identifier runs
            // instead of decoding and pushing one character at a time.
            let bytes = chunk.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                let start = index;
                while index < bytes.len() && ascii_identifier_byte(bytes[index]) {
                    index += 1;
                }
                if index > start {
                    if !oversized {
                        if word.len() + (index - start) <= MAX_COMPLETION_WORD_BYTES {
                            word.push_str(&chunk[start..index]);
                        } else {
                            scores.defer_candidate();
                            word.clear();
                            oversized = true;
                        }
                    }
                    if index == bytes.len() {
                        break;
                    }
                }
                // bytes[index] ends the identifier.
                if !word.is_empty() {
                    scores.add(word, proximity, false, cancelled)?;
                    word.clear();
                }
                oversized = false;
                index += 1;
            }
            continue;
        }
        for ch in chunk.chars() {
            if identifier_char(ch) {
                if !oversized {
                    if word.len() + ch.len_utf8() <= MAX_COMPLETION_WORD_BYTES {
                        word.push(ch);
                    } else {
                        scores.defer_candidate();
                        word.clear();
                        oversized = true;
                    }
                }
            } else {
                if !word.is_empty() {
                    scores.add(word, proximity, false, cancelled)?;
                    word.clear();
                }
                oversized = false;
            }
        }
    }
    if !word.is_empty() {
        scores.add(word, proximity, false, cancelled)?;
        word.clear();
    }
    (!cancelled()).then_some(true)
}

#[derive(Default)]
struct CompletionCache {
    document: Option<u64>,
    prediction: prediction::Cache,
    edit_history: edit_history::Cache,
}

impl CompletionCache {
    fn use_document(&mut self, document: u64) {
        if self.document != Some(document) {
            *self = Self {
                document: Some(document),
                ..Self::default()
            };
        }
    }
}

#[derive(Clone, Copy, Default)]
struct CompletionEvidence {
    generated: Option<patterns::Kind>,
    context_score: i64,
    frequency: u32,
    local: bool,
    semantic_score: i64,
    semantic_seen: bool,
    lexical_seen: bool,
    local_matched_calls: u32,
    local_matched_uses: u32,
    code_uses: u32,
    code_calls: u32,
    domain: crate::completion_context::Domain,
    declaration: bool,
    type_position: bool,
    history_score: i64,
    edit_history: bool,
    local_context_score: i64,
    example_seen: bool,
    example_probability: f64,
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
}

fn local_observations(
    text: &str,
    candidates: &FxHashMap<String, VtCompleteCandidate>,
) -> Vec<LocalObservation> {
    use crate::completion_context::{Analyzer, Event};
    let mut analyzer = Analyzer::default();
    let mut observed: Vec<LocalObservation> = Vec::new();
    let mut positions = text.char_indices().enumerate().peekable();
    let mut record = |event: Event<'_>| {
        if let Event::Word {
            word,
            context,
            offset,
        } = event
            && candidates.contains_key(word)
        {
            while positions.peek().is_some_and(|&(at, _)| at < offset) {
                positions.next();
            }
            let byte = positions.peek().map_or(text.len(), |&(_, (byte, _))| byte);
            observed.push(LocalObservation {
                word: word.to_owned(),
                context: ContextKey::new(&text[..byte]),
                domain: context.domain,
            });
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

#[cfg(test)]
fn compute_contextual(
    active: &VtCompleteSnapshot,
    prefix: &str,
    language: &str,
    options: &PopupOptions,
    cache: &mut CompletionCache,
    cancelled: impl Fn() -> bool,
) -> Option<CompletionResult> {
    compute_contextual_invoked(active, prefix, language, options, cache, true, cancelled)
}

fn compute_contextual_invoked(
    active: &VtCompleteSnapshot,
    prefix: &str,
    language: &str,
    options: &PopupOptions,
    cache: &mut CompletionCache,
    explicit: bool,
    cancelled: impl Fn() -> bool,
) -> Option<CompletionResult> {
    cache.use_document(active.document_id);
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
    let use_edit_history = options.preferences.use_history;
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
    analysis.context.source_family = active.source.kind.family;
    analysis.context.source_scope = active.source.file;
    if !explicit && analysis.context.domain == crate::completion_context::Domain::String {
        return Some(CompletionResult {
            suggestions: Vec::new(),
            details: Vec::new(),
            outcome: VtCompleteWorkOutcome::Exact,
            deferred_candidates: 0,
        });
    }
    let (example_left, example_right, file_before, file_after) = example_context(active, prefix);
    let example_query = completion_examples::Query::new(&example_left, &example_right);
    let mut examples = completion_examples::Evidence::default();
    examples.collect(&example_query, &example_left, true);
    examples.collect(&example_query, &file_before, false);
    examples.collect(&example_query, &file_after, false);
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
    if examples.limited {
        scores.defer_candidate();
    }
    let mut word = String::new();
    let mut context_bytes = 0usize;
    // Search outward from the caret. The token budget also bounds work on
    // very large files; only nearby lines need richer context analysis.
    let line_count = active.rope.len_lines();
    let last_line = line_count - 1;
    let max_distance = active.cursor_line.max(last_line - active.cursor_line);
    // Two cursors walk outward line by line. Looking each line up by index
    // descended the rope again per line and dominated typing on large files.
    let start = (active.cursor_line + 1).min(line_count);
    let mut above = active.rope.lines_at(start);
    let mut below = active.rope.lines_at(start);
    'file: for distance in 0..=max_distance {
        let up = active
            .cursor_line
            .checked_sub(distance)
            .filter(|&line| line < line_count)
            .and_then(|_| above.prev());
        let down = (distance > 0 && active.cursor_line.saturating_add(distance) < line_count)
            .then(|| below.next())
            .flatten();
        for source in [up, down].into_iter().flatten() {
            if !scan_words(source, distance, &mut scores, &mut word, &cancelled)? {
                break 'file;
            }
            if distance < 128
                && source.len_bytes() <= 4096
                && context_bytes + source.len_bytes() <= 48 * 1024
            {
                context_bytes += source.len_bytes();
                for observed in local_observations(&source.to_string(), &scores.matches) {
                    let item = evidence.entry(observed.word).or_default();
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
        if observed.context.domain == analysis.context.domain {
            // Nearby usage supplies a domain-conditioned prior. Code
            // frequency should not displace literal words inside quotes.
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
        }
    }
    if use_edit_history && let Some(history) = &options.history {
        for (word, boost) in history.scores(prefix, analysis.context, explicit) {
            if scores.matches.contains_key(&word) {
                evidence.entry(word).or_default().history_score = boost;
            }
        }
    }
    for word in examples.candidates() {
        scores.add(word, 0, false, &cancelled)?;
        let item = evidence.entry(word.to_owned()).or_default();
        item.example_probability = examples.probability(word, 0.);
        item.example_seen = item.example_probability >= 0.25;
        item.example_reference = examples.call_probability(word).is_some_and(|p| p < 0.5);
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
    if !options.preferences.complete_calls
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
            value.local_matched_calls = 0;
            value.code_calls = 0;
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
            &examples,
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

/// Visible bounded context only. Skip the complete identifier at the caret in
/// the right context, including its untyped tail during replacement edits.
fn example_context(active: &VtCompleteSnapshot, prefix: &str) -> (String, String, String, String) {
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

#[cfg(test)]
#[path = "vtcomplete_numeric_tests.rs"]
mod numeric_tests;
#[cfg(test)]
#[path = "vtcomplete_quality_tests.rs"]
mod quality_tests;
#[cfg(test)]
#[path = "vtcomplete_worker_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "vtcomplete_generation_replay_tests.rs"]
mod generation_replay_tests;
#[cfg(test)]
#[path = "vtcomplete_transform_replay_tests.rs"]
mod transform_replay_tests;
#[cfg(test)]
#[path = "vtcomplete_vtcode_capacity_tests.rs"]
mod vtcode_capacity_tests;

#[cfg(test)]
mod recall {
    thread_local! {
        static TRACE: std::cell::RefCell<(Option<String>, bool)> =
            const { std::cell::RefCell::new((None, false)) };
    }

    pub(super) fn begin_trace(expected: Option<&str>) {
        TRACE.with(|trace| *trace.borrow_mut() = (expected.map(str::to_owned), false));
    }

    pub(super) fn trace_candidates<'a>(mut words: impl Iterator<Item = &'a str>) {
        TRACE.with(|trace| {
            let mut trace = trace.borrow_mut();
            if let Some(expected) = &trace.0 {
                trace.1 = words.any(|word| word == expected);
            }
        });
    }

    pub(super) fn candidate_recalled() -> bool {
        TRACE.with(|trace| trace.borrow().1)
    }
}
