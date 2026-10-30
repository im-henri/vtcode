use ropey::Rope;
use rustc_hash::FxHashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::textmate::Color;
use crate::textmate::{Style, Theme, ThemeSet};
use crate::textmate::{Syntax, SyntaxRegistry};

/// Parser states are cheap enough to retain periodically and let an edit
/// restart at the closest preceding checkpoint instead of line zero.
const CHECKPOINT_INTERVAL: usize = 256;
/// A worker yields regularly even when it is chasing a viewport far into a
/// large file. The next event-loop pass resumes from the checkpoint it returned.
const MAX_LINES_PER_BATCH: usize = 8_192;
const DEFAULT_WORK_BUDGET: Duration = Duration::from_millis(40);
/// If the nearest authoritative parser checkpoint is farther away than this,
/// paint the viewport from a fresh TextMate state while the exact state catches
/// up in the background. The provisional spans are replaced automatically.
const PROVISIONAL_DISTANCE_LINES: usize = 256;
const MAX_CACHED_LINES_PER_DOCUMENT: usize = 5_000;
const CACHED_LINES_AFTER_EVICTION: usize = 4_000;
const MAX_CACHED_DOCUMENTS: usize = 16;
const MAX_CACHED_LINE_BYTES_PER_DOCUMENT: usize = 8 * 1024 * 1024;
const CACHED_LINE_BYTES_AFTER_EVICTION: usize = 6 * 1024 * 1024;
const MAX_CACHED_LINE_BYTES: usize = 32 * 1024 * 1024;
#[cfg(test)]
thread_local! {
    static REFERENCE_EAGER_PREFIX: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REFERENCE_ALL_CHECKPOINTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
#[path = "highlight_load_tests.rs"]
mod load_tests;
#[cfg(test)]
#[path = "highlight_coordinate_tests.rs"]
mod coordinate_tests;
#[cfg(test)]
#[path = "highlight_memory_tests.rs"]
mod memory_tests;
/// TextMate is regex-driven and can spend seconds on a single minified line.
/// Highlight a useful prefix and deliberately reset parser state afterward;
/// the viewport renderer likewise bounds work for these pathological lines.
pub(crate) const MAX_SYNTAX_LINE_CHARS: usize = 16 * 1024;
/// First-line syntax detection only needs shebangs/modelines near the start.
pub(crate) const MAX_SYNTAX_DETECTION_CHARS: usize = 4 * 1024;

type InstalledSyntaxSnapshot = (Arc<SyntaxRegistry>, Option<u64>);
static INSTALLED_SYNTAX_SET: OnceLock<Result<InstalledSyntaxSnapshot, String>> = OnceLock::new();

pub(crate) type HighlightSpans = Vec<(Style, String)>;

// Count owned capacities, not just text length: highly tokenized rows can
// retain more run metadata than text. Rc clones conservatively count the same
// allocation again, so sharing can only reduce actual retained memory.
fn span_bytes(spans: &HighlightSpans) -> usize {
    std::mem::size_of::<HighlightSpans>() + 2 * std::mem::size_of::<usize>()
        + spans.capacity() * std::mem::size_of::<(Style, String)>()
        + spans.iter().map(|(_, text)| text.capacity()).sum::<usize>()
}
#[path = "highlight_fragments.rs"]
mod fragments;
use fragments::FragmentCache;
pub(crate) use fragments::FragmentHighlighter;
#[path = "highlight_line_index.rs"]
mod line_index;
use line_index::LineIndex;

pub(crate) use crate::textmate_backend::State as TextMateState;

#[derive(Clone)]
struct CachedCheckpoint {
    version: u64,
    // Immutable parser snapshots are shared with jobs. Preparing an edit must
    // not deep-copy every suffix checkpoint on the editor thread.
    state: Arc<TextMateState>,
    /// Document coordinate minus the worker's parser offset when published.
    /// Keeping this origin stable makes the current offset derivable from the
    /// index key without rewriting every checkpoint after a newline edit.
    coordinate_origin: i128,
}

impl CachedCheckpoint {
    fn line_offset(&self, line: usize) -> i128 { line as i128 - self.coordinate_origin }
}

#[derive(Clone)]
struct CachedLine {
    version: u64,
    /// Rendering stale text safely does not validate its incoming parser state.
    display_version: u64,
    hash: u64,
    spans: Rc<HighlightSpans>,
    span_char_lengths: Rc<Vec<usize>>,
    char_len: usize,
    comment_ranges: Rc<Vec<(usize, usize)>>,
    authoritative: bool,
    bytes: usize,
}

/// One validated line snapshot supplies glyph runs and comment metadata to the
/// renderer. All components come from the same document/context revision.
pub(crate) struct RenderedLine {
    pub(crate) spans: Rc<HighlightSpans>,
    pub(crate) lengths: Rc<Vec<usize>>,
    pub(crate) char_len: usize,
    pub(crate) comments: Rc<Vec<(usize, usize)>>,
}

impl CachedLine {
    fn from_entry(entry: HighlightEntry, version: u64, authoritative: bool) -> Self {
        let mut line = Self {
            version,
            display_version: version,
            hash: entry.hash,
            char_len: entry.span_char_lengths.iter().sum(),
            spans: Rc::new(entry.spans),
            span_char_lengths: Rc::new(entry.span_char_lengths),
            comment_ranges: Rc::new(entry.comment_ranges),
            authoritative,
            bytes: 0,
        };
        line.bytes = line.allocation_bytes();
        line
    }

    fn allocation_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + span_bytes(&self.spans)
            + 2 * (std::mem::size_of::<Vec<usize>>() + 2 * std::mem::size_of::<usize>())
            + self.span_char_lengths.capacity() * std::mem::size_of::<usize>()
            + self.comment_ranges.capacity() * std::mem::size_of::<(usize, usize)>()
    }

    fn rendered(&self) -> RenderedLine {
        RenderedLine {
            spans: Rc::clone(&self.spans), lengths: Rc::clone(&self.span_char_lengths),
            char_len: self.char_len, comments: Rc::clone(&self.comment_ranges),
        }
    }
}

struct DocumentCache {
    theme_key: String,
    language: Option<String>,
    syntax_name: String,
    syntax_index: usize,
    syntax_generation: u64,
    first_line: Arc<str>,
    version: u64,
    line_count: usize,
    /// The earliest line whose outgoing parser state has not converged with
    /// the pre-edit state yet. `None` means every retained suffix is reusable.
    dirty_from: Option<usize>,
    /// Last line directly touched by the edit batch. State equality before
    /// this line cannot prove that a later multi-cursor edit is unaffected.
    dirty_through: Option<usize>,
    /// Entries before the latest edit remain valid without rewriting thousands
    /// of version stamps on the UI thread. Keep this bound after convergence:
    /// unvisited entries in a changed suffix may still contain old styles.
    unchanged_prefix: usize,
    checkpoints: LineIndex<CachedCheckpoint, 0>,
    lines: LineIndex<CachedLine>,
    line_bytes: usize,
    last_used: u64,
}

impl DocumentCache {
    #[allow(clippy::too_many_arguments)]
    fn new(
        theme_key: &str,
        language: Option<&str>,
        syntax_name: &str,
        version: u64,
        line_count: usize,
        syntax_generation: u64,
        first_line: &Arc<str>,
        syntax_index: usize,
    ) -> Self {
        Self {
            theme_key: theme_key.to_string(),
            language: language.map(str::to_string),
            syntax_name: syntax_name.to_string(),
            syntax_index,
            syntax_generation,
            first_line: Arc::clone(first_line),
            version,
            line_count,
            dirty_from: None,
            dirty_through: None,
            unchanged_prefix: 0,
            checkpoints: LineIndex::default(),
            lines: LineIndex::default(),
            line_bytes: 0,
            last_used: 0,
        }
    }

    fn context_matches(&self, theme_key: &str, language: Option<&str>, syntax_name: &str) -> bool {
        self.theme_key == theme_key
            && self.language.as_deref() == language
            && self.syntax_name == syntax_name
    }

    fn insert_line(&mut self, line: usize, value: CachedLine) {
        self.line_bytes += value.bytes;
        if let Some(previous) = self.lines.insert(line, value) { self.line_bytes -= previous.bytes; }
    }

    fn trim_lines(&mut self, visible: &Range<usize>) {
        if self.lines.len() <= MAX_CACHED_LINES_PER_DOCUMENT
            && self.line_bytes <= MAX_CACHED_LINE_BYTES_PER_DOCUMENT { return; }
        let center = visible.start + visible.end.saturating_sub(visible.start) / 2;
        let mut by_distance = Vec::with_capacity(self.lines.len());
        self.lines.for_each(.., |line, _| {
            if !visible.contains(&line) { by_distance.push((line, line.abs_diff(center))); }
        });
        by_distance.sort_unstable_by_key(|(_, distance)| std::cmp::Reverse(*distance));
        for (line, _) in by_distance {
            if self.lines.len() <= CACHED_LINES_AFTER_EVICTION
                && self.line_bytes <= CACHED_LINE_BYTES_AFTER_EVICTION { break; }
            if let Some(previous) = self.lines.remove(&line) { self.line_bytes -= previous.bytes; }
        }
        // The current viewport may exceed the budget. Keep it usable instead
        // of making the scheduler repeatedly reparse rows we just evicted.
    }

    fn render_context_matches(&self, theme_key: &str, language: Option<&str>) -> bool {
        self.theme_key == theme_key && self.language.as_deref() == language
    }

    fn line_is_current(&self, line: usize, version: u64) -> bool {
        line < self.unchanged_prefix || version == self.version
    }

    fn checkpoint_is_current(&self, line: usize, version: u64) -> bool {
        // Checkpoints describe the parser state *before* their source line.
        line <= self.unchanged_prefix || version == self.version
    }

    /// Carry unchanged prefix entries into a new content version and retain
    /// the shifted suffix as convergence candidates. Suffix entries are not
    /// renderable until a worker proves that TextMate's state matches again.
    fn advance_version(
        &mut self,
        version: u64,
        rope: &Rope,
        changed_from: usize,
        changed_through: usize,
    ) {
        let line_count = rope.len_lines();
        let changed_from = changed_from.min(line_count);
        let changed_through = changed_through.max(changed_from).min(line_count);
        let effective_dirty = self
            .dirty_from
            .map_or(changed_from, |old| old.min(changed_from));
        let delta = line_count as i128 - self.line_count as i128;
        // While a previous propagation is still pending, a later insertion
        // can move its last dirty line down. Editor ranges intentionally stay
        // conservative, so carry positive line deltas into that boundary;
        // deletions may leave it farther down than necessary but never permit
        // convergence before an unparsed edit.
        let carried_through = self
            .dirty_through
            .map(|old| old.saturating_add(delta.max(0) as usize).min(line_count));
        let effective_through =
            carried_through.map_or(changed_through, |old| old.max(changed_through));

        self.version = version;
        self.line_count = line_count;
        self.dirty_from = Some(effective_dirty);
        self.dirty_through = Some(effective_through);
        self.unchanged_prefix = effective_dirty;
        if delta == 0 {
            // Ordinary typing changes neither coordinates nor the prefix.
            // Validate retained entries lazily when a viewport requests them.
            #[cfg(test)]
            if REFERENCE_EAGER_PREFIX.get() {
                self.lines.for_each_mut(..effective_dirty, |_, cached| {
                    cached.version = version;
                    cached.display_version = version;
                });
                self.checkpoints.for_each_mut(..=effective_dirty, |_, checkpoint| {
                    checkpoint.version = version;
                });
                self.unchanged_prefix = 0;
            }
            return;
        }

        let mut dirty_line_candidate = self.lines.get(&changed_from).cloned();
        if delta < 0 && changed_from < line_count {
            let old_continuation = changed_from.saturating_add((-delta) as usize);
            if let Some(continuation) = self.lines.get(&old_continuation) {
                // A deletion can move an already-colored line onto the dirty
                // boundary (notably Undo after inserting a blank line). Keep
                // that entry instead of projecting the deleted blank's style.
                // Inspect just one bounded line; never diff the whole Rope.
                let line = rope.line(changed_from);
                let current = line
                    .slice(..line.len_chars().min(MAX_SYNTAX_LINE_CHARS))
                    .to_string();
                let hash = Highlighter::line_hash(&normalized_line_prefix(&current));
                let boundary_is_blank = dirty_line_candidate.as_ref().is_none_or(|cached| {
                    cached
                        .spans
                        .iter()
                        .all(|(_, text)| text.chars().all(char::is_whitespace))
                });
                if continuation.hash == hash || boundary_is_blank {
                    dirty_line_candidate = Some(continuation.clone());
                }
            }
        }
        self.lines.shift_after_with_removed(changed_from, delta, line_count, |removed| {
            self.line_bytes -= removed.bytes;
        });
        if let Some(candidate) = dirty_line_candidate {
            // A newline splits the old dirty line into an upper fragment and
            // a shifted continuation. Keep the old style runs as candidates
            // for both; rendering projects them onto current text and never
            // shows stale characters. Inserting the original coordinate last
            // also makes line joins deterministic instead of depending on
            // hash-map iteration order at the collision point.
            if delta > 0 {
                if let Some(shifted) = shift_line(changed_from, delta, line_count) {
                    self.insert_line(shifted, candidate.clone());
                }
            }
            self.insert_line(changed_from, candidate);
        }

        // Checkpoints inside deleted rows cannot be used to converge before
        // dirty_through anyway. Drop them rather than collide with unchanged
        // prefix coordinates while another edit is still propagating.
        self.checkpoints.shift_after(changed_from, delta, line_count);
        self.trim_lines(&(changed_from..changed_from.saturating_add(1)));
    }
}

fn shift_line(line: usize, delta: i128, new_line_count: usize) -> Option<usize> {
    let shifted = line as i128 + delta;
    if shifted < 0 || shifted > new_line_count as i128 {
        None
    } else {
        Some(shifted as usize)
    }
}

/// TextMate states contain no absolute line counters. A shifted suffix can be
/// reused once its rule stack and scopes converge in the same parser session.
fn states_converge_after_shift(current: &TextMateState, _current_line_offset: i128,
    previous: &TextMateState, _previous_line_offset: i128) -> bool { current == previous }

#[derive(Clone)]
pub(crate) struct HighlightJob {
    registry_generation: u64,
    file_path: PathBuf,
    theme_key: String,
    language: Option<String>,
    syntax_name: String,
    syntax_index: usize,
    content_version: u64,
    line_count: usize,
    visible: Range<usize>,
    parse_start: usize,
    start_state: Option<Arc<TextMateState>>,
    start_line_offset: i128,
    invalidated_through: Option<usize>,
    stale_checkpoints: Vec<(usize, CachedCheckpoint)>,
    first_line: Arc<str>,
    provisional: bool,
}

#[cfg_attr(test, derive(Clone))]
pub(crate) struct HighlightEntry {
    pub line: usize,
    pub hash: u64,
    pub spans: HighlightSpans,
    span_char_lengths: Vec<usize>,
    comment_ranges: Vec<(usize, usize)>,
}

#[cfg_attr(test, derive(Clone))]
pub(crate) struct HighlightBatch {
    registry_generation: u64,
    pub file_path: PathBuf,
    pub theme_key: String,
    pub language: Option<String>,
    syntax_name: String,
    pub content_version: u64,
    line_count: usize,
    visible: Range<usize>,
    parsed_to: usize,
    checkpoints: Vec<(usize, i128, Arc<TextMateState>)>,
    pub entries: Vec<HighlightEntry>,
    converged_at: Option<usize>,
    provisional: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HighlightApplyOutcome {
    accepted: bool,
    visual_changes: bool,
}

impl HighlightApplyOutcome {
    pub(crate) fn accepted(self) -> bool {
        self.accepted
    }

    pub(crate) fn visual_changes(self) -> bool {
        self.visual_changes
    }
}

impl HighlightJob {
    fn hard_end(parse_start: usize, visible: &Range<usize>, line_count: usize, provisional: bool) -> usize {
        let nominal = parse_start.saturating_add(MAX_LINES_PER_BATCH);
        if provisional || visible.start <= nominal { visible.end.min(line_count) }
        else { nominal.min(line_count) }
    }

    /// Run a bounded, resumable TextMate pass. Every returned checkpoint is the
    /// parser/highlighter state *before* its line index.
    pub(crate) fn run(
        self,
        rope: &Rope,
        syntax_set: &SyntaxRegistry,
        theme_set: &ThemeSet,
        mut cancelled: impl FnMut() -> bool,
    ) -> HighlightBatch {
        self.run_with_budget(
            rope,
            syntax_set,
            theme_set,
            &mut cancelled,
            DEFAULT_WORK_BUDGET,
        )
    }

    pub(crate) fn run_with_budget(
        self,
        rope: &Rope,
        syntax_set: &SyntaxRegistry,
        theme_set: &ThemeSet,
        cancelled: &mut impl FnMut() -> bool,
        budget: Duration,
    ) -> HighlightBatch {
        let Some(theme) = select_theme(theme_set, &self.theme_key) else {
            return self.plain_batch(rope);
        };
        // Preparation selected this grammar from the same immutable registry.
        // In particular, extensionless documents must not run the complete
        // first-line regex catalog again for every resumable worker slice.
        let syntax = syntax_set.syntaxes().get(self.syntax_index)
            .filter(|syntax| syntax.name == self.syntax_name)
            .unwrap_or_else(|| select_syntax(syntax_set, Some(&self.file_path),
                self.language.as_deref(), &self.first_line));
        let mut state = self
            .start_state
            .as_deref()
            .cloned()
            .unwrap_or_else(|| TextMateState::new(syntax, theme));

        let started = Instant::now();
        // Once a stateful pass reaches the requested viewport, finish that
        // viewport before publishing. Applying half a viewport and the other
        // half one frame later makes colors appear to ripple during edits.
        // This work remains cancellable per line and stays off the UI thread.
        let hard_end = Self::hard_end(self.parse_start, &self.visible,
            self.line_count.min(rope.len_lines()), self.provisional);
        let mut checkpoints = Vec::new();
        let mut entries = Vec::with_capacity(self.visible.end.saturating_sub(self.visible.start));
        let mut line = self.parse_start.min(hard_end);
        let mut converged_at = None;
        let mut stale_index = 0usize;
        let mut text = String::new();

        while line < hard_end {
            // Provisional work is already bounded to the viewport. Completing
            // it in one worker pass avoids repeatedly repainting only its first
            // few lines when an expensive grammar exhausts the normal slice.
            let inside_visible = self.visible.contains(&line);
            if cancelled() || (!self.provisional && !inside_visible && started.elapsed() >= budget)
            {
                break;
            }

            while self
                .stale_checkpoints
                .get(stale_index)
                .is_some_and(|(candidate, _)| *candidate < line)
            {
                stale_index += 1;
            }
            let stale_here = self
                .stale_checkpoints
                .get(stale_index)
                .filter(|(candidate, _)| *candidate == line);
            let should_checkpoint = line == self.parse_start
                || line == self.visible.start
                || line.is_multiple_of(CHECKPOINT_INTERVAL)
                || stale_here.is_some();

            if should_checkpoint {
                let may_converge = self.invalidated_through.is_some_and(|dirty| line > dirty);
                if may_converge
                    && stale_here.is_some_and(|(_, previous)| {
                        states_converge_after_shift(
                            &state,
                            self.start_line_offset,
                            &previous.state,
                            previous.line_offset(line),
                        )
                    })
                {
                    converged_at = Some(line);
                    checkpoints.push((line, self.start_line_offset, Arc::new(state.clone())));
                    break;
                }
                checkpoints.push((line, self.start_line_offset, Arc::new(state.clone())));
            }

            let rope_line = rope.line(line);
            if rope_line.len_chars() > MAX_SYNTAX_LINE_CHARS {
                if self.visible.contains(&line) {
                    let mut text = rope_line.slice(..MAX_SYNTAX_LINE_CHARS).to_string();
                    Highlighter::normalize_line_for_highlighting(&mut text);
                    let hash = Highlighter::line_hash(&text);
                    let mut comment_ranges = Vec::new();
                    let spans = state.line(&text,true,&mut comment_ranges).into_iter()
                        .map(|(style,text)|(style,text.to_owned())).collect();
                    entries.push(HighlightEntry {
                        line,
                        hash,
                        span_char_lengths: span_char_lengths(&spans),
                        comment_ranges,
                        spans,
                    });
                }
                // Carrying a partially parsed state into later lines would be
                // misleading. A fresh state gives fast, deterministic recovery
                // on the next ordinary line.
                state = TextMateState::new(syntax, theme);
                line += 1;
                continue;
            }

            text.clear();
            for chunk in rope_line.chunks() {
                text.push_str(chunk);
            }
            Highlighter::normalize_line_for_highlighting(&mut text);
            let hash = if inside_visible {
                Highlighter::line_hash(&text)
            } else {
                0
            };
            let mut comment_ranges = Vec::new();
            let ranges=state.line(&text,inside_visible,&mut comment_ranges);
            if inside_visible {
                let spans:HighlightSpans=ranges.into_iter().map(|(style,text)|(style,text.to_owned())).collect();
                entries.push(HighlightEntry {line,hash,span_char_lengths:span_char_lengths(&spans),comment_ranges,spans});
            }
            line += 1;
        }

        if converged_at.is_none() {
            if checkpoints.last().map(|(at, _, _)| *at) != Some(line) {
                checkpoints.push((line, self.start_line_offset, Arc::new(state.clone())));
            }
            let may_converge = self.invalidated_through.is_some_and(|dirty| line > dirty);
            if may_converge
                && self
                    .stale_checkpoints
                    .iter()
                    .find(|(candidate, _)| *candidate == line)
                    .is_some_and(|(_, previous)| {
                        states_converge_after_shift(
                            &state,
                            self.start_line_offset,
                            &previous.state,
                            previous.line_offset(line),
                        )
                    })
            {
                converged_at = Some(line);
            }
        }

        HighlightBatch {
            registry_generation: self.registry_generation,
            file_path: self.file_path,
            theme_key: self.theme_key,
            language: self.language,
            syntax_name: self.syntax_name,
            content_version: self.content_version,
            line_count: self.line_count,
            visible: self.visible,
            parsed_to: line,
            checkpoints,
            entries,
            converged_at,
            provisional: self.provisional,
        }
    }

    fn plain_batch(self, rope: &Rope) -> HighlightBatch {
        let entries = self
            .visible
            .clone()
            .filter(|line| *line < rope.len_lines())
            .map(|line| {
                let rope_line = rope.line(line);
                let mut text = if rope_line.len_chars() > MAX_SYNTAX_LINE_CHARS {
                    rope_line.slice(..MAX_SYNTAX_LINE_CHARS).to_string()
                } else {
                    rope_line.to_string()
                };
                Highlighter::normalize_line_for_highlighting(&mut text);
                HighlightEntry {
                    line,
                    hash: Highlighter::line_hash(&text),
                    span_char_lengths: vec![
                        text.strip_suffix('\n').unwrap_or(&text).chars().count(),
                    ],
                    spans: vec![(Style::default(), text)],
                    comment_ranges: Vec::new(),
                }
            })
            .collect();
        HighlightBatch {
            registry_generation: self.registry_generation,
            file_path: self.file_path,
            theme_key: self.theme_key,
            language: self.language,
            syntax_name: self.syntax_name,
            content_version: self.content_version,
            line_count: self.line_count,
            visible: self.visible,
            parsed_to: self.line_count,
            checkpoints: Vec::new(),
            entries,
            converged_at: None,
            provisional: self.provisional,
        }
    }
}

impl HighlightBatch {
    pub(crate) fn is_provisional(&self) -> bool {
        self.provisional
    }

    #[cfg(test)]
    pub(crate) fn has_render_entries(&self) -> bool {
        !self.entries.is_empty()
    }
}

fn select_theme<'a>(theme_set: &'a ThemeSet, theme_key: &str) -> Option<&'a Theme> {
    theme_set
        .themes
        .get(theme_key)
        .or_else(|| theme_set.themes.get("dark-plus"))
        .or_else(|| theme_set.themes.values().next())
}

fn span_char_lengths(spans: &HighlightSpans) -> Vec<usize> {
    spans
        .iter()
        .map(|(_, text)| text.strip_suffix('\n').unwrap_or(text).chars().count())
        .collect()
}

/// Build the exact bounded text used by worker cache keys without running a
/// grammar on the UI thread.
fn normalized_line_prefix(line: &str) -> String {
    let mut normalized = String::with_capacity(line.len().min(MAX_SYNTAX_LINE_CHARS + 1));
    normalized.extend(line.chars().take(MAX_SYNTAX_LINE_CHARS));
    Highlighter::normalize_line_for_highlighting(&mut normalized);
    normalized
}

/// Keep the current text on screen while borrowing the previous line's style
/// runs. Common prefix/suffix characters retain their exact old style; newly
/// inserted or replaced text inherits the nearest prior run until TextMate's
/// authoritative worker result arrives.
fn project_stale_spans(previous: &HighlightSpans, current: &str) -> HighlightSpans {
    let current = current.strip_suffix('\n').unwrap_or(current);
    let current_len = current.chars().count();
    let runs: Vec<_> = previous.iter().map(|(style, text)|
        (*style, text.trim_end_matches(['\r', '\n']).chars().count())).collect();
    let previous_len: usize = runs.iter().map(|(_, len)| len).sum();
    let previous_chars = || previous.iter().flat_map(|(_, text)| text.trim_end_matches(['\r', '\n']).chars());
    let prefix = previous_chars().zip(current.chars()).take_while(|(a, b)| a == b).count();
    let suffix = previous_chars().rev().zip(current.chars().rev())
        .take(previous_len.min(current_len).saturating_sub(prefix))
        .take_while(|(a, b)| a == b).count();
    let inherited = {
        let mut index = prefix.saturating_sub(1);
        runs.iter().find_map(|&(style, len)| {
            if index < len { Some(style) } else { index -= len; None }
        }).unwrap_or_default()
    };
    let ascii = current.is_ascii();
    let prefix_end = style_split_at(current, prefix, ascii);
    let middle_end = prefix_end + style_split_at(&current[prefix_end..], current_len - prefix - suffix, ascii);
    let mut projected = Vec::with_capacity(previous.len().max(1));
    project_style_runs(&mut projected, &current[..prefix_end], &runs, 0, prefix, inherited, ascii);
    project_style_runs(&mut projected, &current[prefix_end..middle_end], &runs,
        prefix, previous_len - prefix - suffix, inherited, ascii);
    project_style_runs(&mut projected, &current[middle_end..], &runs,
        previous_len - suffix, suffix, inherited, ascii);
    if let Some((_, text)) = projected.last_mut() { text.push('\n'); }
    else { projected.push((inherited, "\n".to_owned())); }
    projected
}

fn style_split_at(text: &str, chars: usize, ascii: bool) -> usize {
    if ascii { chars.min(text.len()) }
    else { text.char_indices().nth(chars).map_or(text.len(), |(byte, _)| byte) }
}

fn push_projected_span(output: &mut HighlightSpans, style: Style, text: &str) {
    if text.is_empty() { return; }
    if let Some((previous, content)) = output.last_mut() && *previous == style {
        content.push_str(text);
    } else {
        output.push((style, text.to_owned()));
    }
}

/// Copy whole style runs instead of allocating a Style for every character.
/// Replacements inherit the old middle; inserted excess keeps its last style.
#[allow(clippy::too_many_arguments)]
fn project_style_runs(output: &mut HighlightSpans, mut text: &str, runs: &[(Style, usize)],
    mut skip: usize, mut remaining: usize, mut inherited: Style, ascii: bool)
{
    for &(style, len) in runs {
        if text.is_empty() || remaining == 0 { break; }
        if skip >= len { skip -= len; continue; }
        let count = (len - skip).min(remaining);
        skip = 0;
        let end = style_split_at(text, count, ascii);
        push_projected_span(output, style, &text[..end]);
        inherited = style;
        text = &text[end..];
        remaining -= count;
    }
    push_projected_span(output, inherited, text);
}

#[cfg(test)]
fn reference_project_stale_spans(previous: &HighlightSpans, current: &str) -> HighlightSpans {
    let current = current.strip_suffix('\n').unwrap_or(current);
    let current_chars: Vec<char> = current.chars().collect();
    let current_len = current_chars.len();
    let mut previous_chars = Vec::new();
    let mut previous_styles = Vec::new();
    for (style, text) in previous {
        let text = text.trim_end_matches(['\r', '\n']);
        for ch in text.chars() {
            previous_chars.push(ch);
            previous_styles.push(*style);
        }
    }

    let mut prefix = 0usize;
    while prefix < previous_chars.len()
        && prefix < current_chars.len()
        && previous_chars[prefix] == current_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < previous_chars.len().saturating_sub(prefix)
        && suffix < current_chars.len().saturating_sub(prefix)
        && previous_chars[previous_chars.len() - 1 - suffix]
            == current_chars[current_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let previous_middle_len = previous_chars.len().saturating_sub(prefix + suffix);
    let inherited = prefix
        .checked_sub(1)
        .and_then(|idx| previous_styles.get(idx).copied())
        .or_else(|| previous_styles.get(prefix).copied())
        .unwrap_or_default();
    let mut projected: HighlightSpans = Vec::with_capacity(previous.len().max(1));
    for (idx, ch) in current_chars.into_iter().enumerate() {
        let style = if idx < prefix {
            previous_styles[idx]
        } else if idx >= current_len.saturating_sub(suffix) {
            let from_end = current_len - idx;
            previous_styles[previous_styles.len() - from_end]
        } else if previous_middle_len > 0 {
            previous_styles[prefix + (idx - prefix).min(previous_middle_len - 1)]
        } else {
            inherited
        };
        if let Some((last_style, text)) = projected.last_mut() {
            if *last_style == style {
                text.push(ch);
                continue;
            }
        }
        projected.push((style, ch.to_string()));
    }
    if let Some((_, text)) = projected.last_mut() {
        text.push('\n');
    } else {
        projected.push((inherited, "\n".to_string()));
    }
    projected
}

pub(crate) fn select_syntax<'a>(
    syntax_set: &'a SyntaxRegistry,
    file_path: Option<&Path>,
    language: Option<&str>,
    first_line: &str,
) -> &'a Syntax {
    language
        .and_then(|name| {
            syntax_set
                .find_syntax_by_name(name)
                .or_else(|| syntax_set.find_syntax_by_token(name))
        })
        .or_else(|| {
            let path = file_path?;
            let file_name = path.file_name()?.to_str()?;
            syntax_set.find_syntax_by_extension(file_name)
        })
        .or_else(|| {
            file_path
                .and_then(Path::extension)
                .and_then(|extension| extension.to_str())
                .and_then(|extension| syntax_set.find_syntax_by_extension(extension))
        })
        .or_else(|| syntax_set.find_syntax_by_first_line(first_line))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text())
}

/// TextMate-backed document highlighter. The UI owns only compact styled-line
/// results; parser/highlighter work and state propagation happen on workers.
pub(crate) struct SyntaxAssetRefresh {
    bootstrap: bool,
    installed_folder: Option<PathBuf>,
    force_install: bool,
    loaded_fingerprint: Option<u64>,
    base_syntax_set: Arc<SyntaxRegistry>,
    candidates: Vec<PathBuf>,
    previous_folders: Vec<PathBuf>,
}

pub(crate) struct LoadedSyntaxAssets {
    installed_folder: Option<PathBuf>,
    base_syntax_set: Arc<SyntaxRegistry>,
    fingerprint: Option<u64>,
    syntax_set: Arc<SyntaxRegistry>,
    folders: Vec<PathBuf>,
    pub(crate) cache_write_error: Option<String>,
    pub(crate) warning: Option<String>,
}

impl SyntaxAssetRefresh {
    /// Validate and rebuild installed assets, then layer custom grammars. This
    /// method performs filesystem traversal and parsing and must stay on a
    /// background worker.
    #[cfg(test)]
    pub(crate) fn run(self) -> Result<Option<LoadedSyntaxAssets>, String> {
        let mut cached = None;
        let refreshed = self.run_with_cached(|loaded| cached = Some(loaded))?;
        Ok(refreshed.or(cached))
    }

    pub(crate) fn run_with_cached(
        mut self, mut publish_cached: impl FnMut(LoadedSyntaxAssets),
    ) -> Result<Option<LoadedSyntaxAssets>, String> {
        if self.bootstrap {
            // Even loading a compiled registry can be slow on a cold disk.
            // Publish it before source validation/custom-grammar compilation so
            // visible files can highlight while that maintenance continues.
            if self.installed_folder.is_none() {
                self.installed_folder = crate::syntax_assets::installed_syntax_folder();
            }
            let cached = crate::syntax_assets::load_startup_syntax_set(self.installed_folder.as_deref())?;
            if cached.fingerprint.is_some() {
                self.base_syntax_set = Arc::new(cached.syntax_set);
                self.loaded_fingerprint = cached.fingerprint;
                self.force_install = false;
                publish_cached(LoadedSyntaxAssets {
                    installed_folder: self.installed_folder.clone(),
                    syntax_set: self.base_syntax_set.clone(),
                    base_syntax_set: self.base_syntax_set.clone(),
                    fingerprint: self.loaded_fingerprint,
                    folders: Vec::new(), cache_write_error: None, warning: None,
                });
            }
        }
        let refreshed = crate::syntax_assets::refresh_installed_syntax_set(
            self.installed_folder.as_deref(),
            self.force_install,
            self.loaded_fingerprint,
        )?;
        let installed_changed = refreshed.is_some();
        let (base_syntax_set, fingerprint, cache_write_error) = refreshed.map_or_else(
            || (self.base_syntax_set, self.loaded_fingerprint, None),
            |refreshed| (
                Arc::new(refreshed.syntax_set),
                refreshed.fingerprint,
                refreshed.cache_write_error,
            ),
        );

        let mut folders = Vec::new();
        for folder in self.candidates {
            if folder.is_dir() && !folders.iter().any(|known| known == &folder) {
                folders.push(folder);
            }
        }
        if folders.is_empty() {
            return Ok((installed_changed || !self.previous_folders.is_empty()).then(|| {
                LoadedSyntaxAssets {
                    installed_folder: self.installed_folder,
                    syntax_set: Arc::clone(&base_syntax_set),
                    base_syntax_set,
                    fingerprint,
                    folders,
                    cache_write_error,
                    warning: None,
                }
            }));
        }

        let mut builder = base_syntax_set.as_ref().clone().into_builder();
        for path in &folders {
            if let Err(error) = crate::syntax_assets::add_syntax_folder(&mut builder, path) {
                if !installed_changed {
                    return Err(error);
                }
                // A broken override must not strand the first launch on the
                // Plain Text fallback after successfully building the cache.
                return Ok(Some(LoadedSyntaxAssets {
                    installed_folder: self.installed_folder,
                    syntax_set: Arc::clone(&base_syntax_set),
                    base_syntax_set,
                    fingerprint,
                    folders: Vec::new(),
                    cache_write_error,
                    warning: Some(format!("Installed syntaxes loaded; custom syntaxes could not be loaded: {error}")),
                }));
            }
        }
        Ok(Some(LoadedSyntaxAssets {
            installed_folder: self.installed_folder,
            base_syntax_set,
            fingerprint,
            syntax_set: Arc::new(builder.build()?),
            folders,
            cache_write_error,
            warning: None,
        }))
    }
}

pub struct Highlighter {
    bootstrap: bool,
    /// Immutable installed product assets. User/workspace folders are layered
    /// over this set without reparsing every shipped grammar.
    base_syntax_set: Arc<SyntaxRegistry>,
    syntax_set: Arc<SyntaxRegistry>,
    scope_automatic: std::cell::OnceCell<Option<Arc<crate::scope_bubble_config::Automatic>>>,
    theme_set: Arc<ThemeSet>,
    documents: FxHashMap<PathBuf, DocumentCache>,
    /// Isolated diff rows are not a coherent document stream, so they use a
    /// small secondary TextMate cache without contaminating editor state.
    fragment_cache: FragmentCache,
    installed_syntax_folder: Option<PathBuf>,
    startup_syntax_cache_loaded: bool,
    installed_syntax_fingerprint: Option<u64>,
    syntax_folders: Vec<PathBuf>,
    registry_generation: u64,
    use_counter: u64,
    #[cfg(test)]
    pub(crate) replace_unchanged_snapshots: bool,
}

impl Highlighter {
    #[cfg(test)]
    pub(crate) fn for_syntax_folder(folder: &Path) -> Self {
        let startup = crate::syntax_assets::load_startup_syntax_set(Some(folder)).unwrap();
        let mut highlighter = Self::new();
        highlighter.base_syntax_set = Arc::new(startup.syntax_set);
        highlighter.syntax_set = Arc::clone(&highlighter.base_syntax_set);
        highlighter.startup_syntax_cache_loaded = startup.fingerprint.is_some();
        highlighter.installed_syntax_fingerprint = startup.fingerprint;
        highlighter.installed_syntax_folder = Some(folder.to_path_buf());
        highlighter
    }

    pub(crate) fn new() -> Self {
        Self::new_with_bootstrap(false)
    }

    pub(crate) fn new_deferred() -> Self {
        Self::new_with_bootstrap(true)
    }

    pub(crate) fn startup_theme_set() -> Arc<ThemeSet> {
        static THEMES: std::sync::OnceLock<Arc<ThemeSet>> = std::sync::OnceLock::new();
        Arc::clone(THEMES.get_or_init(|| {
            let mut themes = ThemeSet::new();
            let dark = crate::app_theme_loader::builtin_theme("dark").unwrap();
            if let Some(theme) = &dark.textmate_theme {
                themes.themes.insert(dark.syntax_theme.clone(), theme.clone());
            }
            Arc::new(themes)
        }))
    }

    #[cfg(test)]
    pub(crate) fn for_startup_syntax_folder(folder: &Path) -> Self {
        let mut highlighter = Self::new_deferred();
        highlighter.installed_syntax_folder = Some(folder.to_path_buf());
        highlighter
    }

    fn new_with_bootstrap(bootstrap: bool) -> Self {
        let theme_set = if bootstrap { Self::startup_theme_set() } else {
            let mut themes = ThemeSet::new();
            themes.themes = Self::startup_theme_set().themes.clone();
            install_bundled_themes(&mut themes);
            Arc::new(themes)
        };
        let installed_folder = if bootstrap { None } else { crate::syntax_assets::installed_syntax_folder() };
        let (base_syntax_set, installed_syntax_fingerprint) = if bootstrap {
            (Arc::new(crate::syntax_assets::build_syntax_set(&[])
                .expect("the Plain Text fallback must load")), None)
        } else {
            match INSTALLED_SYNTAX_SET.get_or_init(|| {
                // Ordinary unit tests need a complete syntax registry even in
                // a fresh checkout without a generated startup cache. Explicit
                // cold-start tests use for_syntax_folder and the real loader.
                #[cfg(test)]
                let startup = crate::syntax_assets::refresh_installed_syntax_set(
                    installed_folder.as_deref(), true, None,
                ).map(|loaded| loaded.expect("forced syntax refresh must return a registry"));
                #[cfg(not(test))]
                let startup = crate::syntax_assets::load_startup_syntax_set(installed_folder.as_deref());
                startup.map(|loaded| (Arc::new(loaded.syntax_set), loaded.fingerprint))
            }) {
                Ok((syntax_set, fingerprint)) => (Arc::clone(syntax_set), *fingerprint),
                Err(error) => {
                    log_warn!("failed to load installed syntax cache: {error}");
                    (
                        Arc::new(
                            crate::syntax_assets::build_syntax_set(&[])
                                .expect("the Plain Text fallback must load"),
                        ),
                        None,
                    )
                }
            }
        };
        Self {
            bootstrap,
            scope_automatic: Default::default(),
            syntax_set: Arc::clone(&base_syntax_set),
            base_syntax_set,
            theme_set,
            documents: FxHashMap::default(),
            fragment_cache: FragmentCache::default(),
            #[cfg(test)]
            replace_unchanged_snapshots: false,
            installed_syntax_folder: installed_folder,
            startup_syntax_cache_loaded: installed_syntax_fingerprint.is_some(),
            installed_syntax_fingerprint,
            syntax_folders: Vec::new(),
            registry_generation: 0,
            use_counter: 0,
        }
    }

    /// Return whether the registered theme changed. Reapplying settings must
    /// preserve cached colors and workers using an identical theme snapshot.
    pub(crate) fn register_theme(&mut self, key: &str, theme: Theme) -> bool {
        if self.theme_set.themes.get(key) == Some(&theme) {
            return false;
        }
        let replacing_existing = self.theme_set.themes.contains_key(key);
        if let Some(theme_set) = Arc::get_mut(&mut self.theme_set) {
            theme_set.themes.insert(key.to_string(), theme);
        } else {
            // Highlight workers may still hold the previous immutable set.
            // Publish a copy-on-write replacement rather than dropping the
            // newly discovered theme or waiting for those workers on the UI.
            let mut theme_set = ThemeSet::new();
            theme_set.themes = self.theme_set.themes.clone();
            theme_set.themes.insert(key.to_string(), theme);
            self.theme_set = Arc::new(theme_set);
        }
        if replacing_existing {
            self.registry_generation = self.registry_generation.wrapping_add(1);
            // Catalog discovery can replace a theme that no visible editor
            // uses. Keep the selected theme's colors while workers restart
            // against the new immutable registry snapshot.
            self.documents.retain(|_, cache| cache.theme_key != key);
            self.fragment_cache.remove_theme(key);
        }
        true
    }

    pub(crate) fn syntax_asset_refresh(&self, candidates: Vec<PathBuf>) -> SyntaxAssetRefresh {
        SyntaxAssetRefresh {
            bootstrap: self.bootstrap,
            installed_folder: self.installed_syntax_folder.clone(),
            force_install: !self.startup_syntax_cache_loaded,
            loaded_fingerprint: self.installed_syntax_fingerprint,
            base_syntax_set: Arc::clone(&self.base_syntax_set),
            candidates,
            previous_folders: self.syntax_folders.clone(),
        }
    }

    pub(crate) fn install_syntax_assets(&mut self, loaded: LoadedSyntaxAssets) {
        self.scope_automatic.take();
        self.bootstrap = false;
        self.installed_syntax_folder = loaded.installed_folder;
        self.base_syntax_set = loaded.base_syntax_set;
        self.installed_syntax_fingerprint = loaded.fingerprint;
        self.syntax_set = loaded.syntax_set;
        self.syntax_folders = loaded.folders;
        self.startup_syntax_cache_loaded = true;
        self.registry_generation = self.registry_generation.wrapping_add(1);
        // Registry changes invalidate parser state, but the existing text and
        // colors remain a valid display fallback until the new batch arrives.
        for cache in self.documents.values_mut() {
            cache.checkpoints.clear();
            cache.dirty_from = Some(0);
            cache.dirty_through = Some(cache.line_count);
            cache.lines.for_each_mut(.., |_, line| {
                line.authoritative = false;
            });
        }
        self.fragment_cache.clear();
    }

    /// Human-facing language modes exposed by the loaded TextMate registry.
    /// Internal helper syntaxes remain available for grammar includes but are
    /// intentionally omitted from UI lists, as requested by TextMate's model.
    pub(crate) fn syntax_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .syntax_set
            .syntaxes()
            .iter()
            .filter(|syntax| !syntax.hidden)
            .map(|syntax| syntax.name.clone())
            .collect();
        names.sort_by_key(|name| name.to_ascii_lowercase());
        names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        names
    }

    pub(crate) fn scope_bubble_automatic(&self) -> Option<Arc<crate::scope_bubble_config::Automatic>> {
        self.scope_automatic.get_or_init(||
            crate::scope_bubble_config::Automatic::new(self.syntax_set.clone())).clone()
    }

    pub(crate) fn line_hash(line: &str) -> u64 {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in line.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash
    }

    /// Rope preserves CRLF. VTCode builds its TextMate set in newline mode, so
    /// every worker and renderer cache key uses exactly one trailing LF.
    pub(crate) fn normalize_line_for_highlighting(line: &mut String) {
        while matches!(line.as_bytes().last().copied(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        line.push('\n');
    }

    /// Synchronize a document cache with an editor version and prepare the
    /// smallest resumable worker job needed for the requested viewport.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_job(
        &mut self,
        rope: &Rope,
        file_path: &Path,
        content_version: u64,
        changed_range: Option<(usize, usize)>,
        visible: Range<usize>,
        language: Option<&str>,
        theme_key: &str,
    ) -> Option<HighlightJob> {
        // Plain text is immediately drawable without building parser caches
        // that would be thrown away when the startup registry arrives.
        if self.bootstrap { return None; }
        self.use_counter = self.use_counter.wrapping_add(1);
        let line_count = rope.len_lines();
        let visible = visible.start.min(line_count)..visible.end.min(line_count);
        if let Some(cache) = self.documents.get_mut(file_path)
            && cache.syntax_generation == self.registry_generation
            && cache.render_context_matches(theme_key, language)
            && cache.version == content_version
            && cache.line_count == line_count
            && changed_range.is_none()
            && visible.clone().all(|line| cache.lines.get(&line)
                .is_some_and(|entry| cache.line_is_current(line, entry.version) && entry.authoritative))
        {
            // A warm viewport needs no line-zero snapshot, syntax detection,
            // owned request strings, or checkpoint search on cursor frames.
            cache.last_used = self.use_counter;
            return None;
        }
        let reuse_detection = self.documents.get(file_path).filter(|cache| {
            cache.syntax_generation == self.registry_generation
                && cache.render_context_matches(theme_key, language)
                && ((cache.version == content_version && changed_range.is_none())
                    || (cache.version != content_version
                        && changed_range.is_some_and(|(start, _)| start > 0)))
        });
        let (first_line, syntax_name, syntax_index) = if let Some(cache) = reuse_detection {
            (cache.first_line.clone(), cache.syntax_name.clone(), cache.syntax_index)
        } else {
            let first_rope_line = rope.line(0);
            let mut first_line = first_rope_line
                .slice(..first_rope_line.len_chars().min(MAX_SYNTAX_DETECTION_CHARS))
                .to_string();
            Self::normalize_line_for_highlighting(&mut first_line);
            let syntax = select_syntax(&self.syntax_set, Some(file_path), language, &first_line);
            let syntax_index = self.syntax_set.syntaxes().iter()
                .position(|candidate| std::ptr::eq(candidate, syntax))
                .expect("selected syntax belongs to the registry");
            (Arc::<str>::from(first_line), syntax.name.clone(), syntax_index)
        };
        let reset = self
            .documents
            .get(file_path)
            .is_none_or(|cache| !cache.context_matches(theme_key, language, &syntax_name)
                || cache.syntax_index != syntax_index);
        if reset {
            self.documents.insert(
                file_path.to_path_buf(),
                DocumentCache::new(
                    theme_key,
                    language,
                    &syntax_name,
                    content_version,
                    line_count,
                    self.registry_generation,
                    &first_line,
                    syntax_index,
                ),
            );
        }

        let cache = self.documents.get_mut(file_path)?;
        cache.last_used = self.use_counter;
        if cache.version == content_version
            && changed_range.is_some()
            && cache.dirty_from.is_none()
            && (!cache.lines.is_empty() || !cache.checkpoints.is_empty())
        {
            // A newly opened editor can restart its local content counter at
            // zero for a path retained in the multi-document cache.
            *cache = DocumentCache::new(
                theme_key,
                language,
                &syntax_name,
                content_version,
                line_count,
                self.registry_generation,
                &first_line,
                syntax_index,
            );
            cache.last_used = self.use_counter;
        }
        cache.syntax_generation = self.registry_generation;
        cache.first_line = Arc::clone(&first_line);
        if cache.version != content_version || cache.line_count != line_count {
            // A missing edit location is treated conservatively as a full reset.
            let (changed_from, changed_through) = changed_range.unwrap_or((0, line_count));
            cache.advance_version(content_version, rope, changed_from, changed_through);
        }

        let visible = visible.start.min(line_count)..visible.end.min(line_count);
        if visible.is_empty() {
            return None;
        }
        let first_missing = visible.clone().find(|line| {
            cache
                .lines
                .get(line)
                .is_none_or(|entry| !cache.line_is_current(*line, entry.version) || !entry.authoritative)
        });
        let needed = first_missing?;

        let (parse_start, start_state, start_line_offset) = cache
            .checkpoints
            .last_before_where(needed, |line, checkpoint| cache.checkpoint_is_current(line, checkpoint.version))
            .map_or((0, None, 0), |(line, checkpoint)| {
                (
                    line,
                    Some(checkpoint.state.clone()),
                    checkpoint.line_offset(line),
                )
            });
        // A slice can only converge at checkpoints it can reach. Copying the
        // entire document suffix on every key/scroll competes with input while
        // the parser is still catching up. Include the final boundary check.
        let hard_end = HighlightJob::hard_end(parse_start, &visible, line_count, false);
        #[cfg(test)]
        let hard_end = if REFERENCE_ALL_CHECKPOINTS.get() { usize::MAX } else { hard_end };
        let mut stale_checkpoints = Vec::new();
        cache.checkpoints.for_each(parse_start..=hard_end, |line, checkpoint| {
            if !cache.checkpoint_is_current(line, checkpoint.version) {
                stale_checkpoints.push((line, checkpoint.clone()));
            }
        });
        Some(HighlightJob {
            registry_generation: self.registry_generation,
            file_path: file_path.to_path_buf(),
            theme_key: theme_key.to_string(),
            language: language.map(str::to_string),
            syntax_name,
            syntax_index,
            content_version,
            line_count,
            visible,
            parse_start,
            start_state,
            start_line_offset,
            invalidated_through: cache.dirty_through,
            stale_checkpoints,
            first_line,
            provisional: false,
        })
    }

    /// Build a bounded best-effort viewport pass when the exact parser state
    /// is still far away. This is still TextMate highlighting; it simply starts
    /// with the grammar's initial state. The authoritative job remains queued
    /// and replaces these lines after resolving multiline context.
    pub(crate) fn prepare_provisional_job(
        &self,
        authoritative: &HighlightJob,
    ) -> Option<HighlightJob> {
        if authoritative.provisional
            || authoritative
                .visible
                .start
                .saturating_sub(authoritative.parse_start)
                <= PROVISIONAL_DISTANCE_LINES
        {
            return None;
        }
        let cache = self.documents.get(&authoritative.file_path)?;
        let needs_visible_spans = authoritative.visible.clone().any(|line| {
            cache
                .lines
                .get(&line)
                .is_none_or(|entry| !cache.line_is_current(line, entry.display_version))
        });
        if !needs_visible_spans {
            return None;
        }

        Some(HighlightJob {
            registry_generation: authoritative.registry_generation,
            file_path: authoritative.file_path.clone(),
            theme_key: authoritative.theme_key.clone(),
            language: authoritative.language.clone(),
            syntax_name: authoritative.syntax_name.clone(),
            syntax_index: authoritative.syntax_index,
            content_version: authoritative.content_version,
            line_count: authoritative.line_count,
            visible: authoritative.visible.clone(),
            parse_start: authoritative.visible.start,
            start_state: None,
            start_line_offset: 0,
            invalidated_through: None,
            stale_checkpoints: Vec::new(),
            first_line: authoritative.first_line.clone(),
            provisional: true,
        })
    }

    /// Merge a worker slice only if the document context/version is still
    /// current. Tests generally care only whether the batch was accepted;
    /// the event loop additionally distinguishes parser progress from an
    /// actual on-screen style change.
    #[cfg(test)]
    pub(crate) fn apply_batch(&mut self, batch: HighlightBatch) -> bool {
        self.apply_batch_with_outcome(batch).accepted()
    }

    pub(crate) fn apply_batch_with_outcome(
        &mut self,
        batch: HighlightBatch,
    ) -> HighlightApplyOutcome {
        if batch.registry_generation != self.registry_generation {
            return HighlightApplyOutcome::default();
        }
        let Some(cache) = self.documents.get_mut(&batch.file_path) else {
            return HighlightApplyOutcome::default();
        };
        if cache.version != batch.content_version
            || cache.line_count != batch.line_count
            || !cache.context_matches(
                &batch.theme_key,
                batch.language.as_deref(),
                &batch.syntax_name,
            )
        {
            return HighlightApplyOutcome::default();
        }

        if batch.provisional {
            let mut visual_changes = false;
            let unchanged_prefix = cache.unchanged_prefix;
            for entry in batch.entries {
                if let Some(cached) = cache.lines.get_mut(&entry.line) {
                    if (entry.line < unchanged_prefix || cached.version == batch.content_version) && cached.authoritative {
                        continue;
                    }
                    if cached.hash == entry.hash {
                        // The visible text is identical. Keep its previous
                        // context-aware colors until the exact stateful pass
                        // arrives; a viewport-local parse may start inside a
                        // multiline string/comment and briefly disagree.
                        cached.display_version = batch.content_version;
                        continue;
                    }
                }
                visual_changes = true;
                cache.insert_line(
                    entry.line,
                    CachedLine::from_entry(entry, batch.content_version, false),
                );
            }
            // Provisional results also accumulate while exact parsing is
            // delayed; they must obey the same retention policy.
            cache.trim_lines(&batch.visible);
            self.evict_old_documents(&batch.file_path);
            return HighlightApplyOutcome {
                accepted: true,
                visual_changes,
            };
        }

        let mut visual_changes = false;
        for (line, line_offset, state) in batch.checkpoints {
            cache.checkpoints.insert(
                line,
                CachedCheckpoint {
                    version: batch.content_version,
                    state,
                    coordinate_origin: line as i128 - line_offset,
                },
            );
        }
        #[cfg(test)]
        let reuse_snapshots = !self.replace_unchanged_snapshots;
        #[cfg(not(test))]
        let reuse_snapshots = true;
        for entry in batch.entries {
            if reuse_snapshots && let Some(cached) = cache.lines.get_mut(&entry.line) {
                let spans_changed = cached.spans.as_ref() != &entry.spans;
                let comments_changed = cached.comment_ranges.as_ref() != &entry.comment_ranges;
                visual_changes |= cached.hash != entry.hash || spans_changed || comments_changed;
                cached.version = batch.content_version;
                cached.display_version = batch.content_version;
                cached.hash = entry.hash;
                cached.authoritative = true;
                // Glyph caches use Rc identity to detect style changes. Keep
                // identical worker results shared so parser progress doesn't
                // force every unchanged visible line to be decoded again.
                if spans_changed {
                    cached.spans = Rc::new(entry.spans);
                    cached.char_len = entry.span_char_lengths.iter().sum();
                    cached.span_char_lengths = Rc::new(entry.span_char_lengths);
                }
                if comments_changed { cached.comment_ranges = Rc::new(entry.comment_ranges); }
                if spans_changed || comments_changed {
                    cache.line_bytes -= cached.bytes;
                    cached.bytes = cached.allocation_bytes();
                    cache.line_bytes += cached.bytes;
                }
                continue;
            }
            visual_changes |= cache.lines.get(&entry.line).is_none_or(|cached| {
                cached.hash != entry.hash || cached.spans.as_ref() != &entry.spans
                    || cached.comment_ranges.as_ref() != &entry.comment_ranges
            });
            cache.insert_line(
                entry.line,
                CachedLine::from_entry(entry, batch.content_version, true),
            );
        }

        if let Some(converged) = batch.converged_at {
            cache.checkpoints.for_each_mut(converged.., |_, checkpoint| {
                checkpoint.version = batch.content_version;
            });
            cache.lines.for_each_mut(converged.., |_, cached| {
                cached.version = batch.content_version;
                cached.display_version = batch.content_version;
            });
            cache.dirty_from = None;
            cache.dirty_through = None;
        } else if batch.parsed_to >= batch.line_count {
            cache.dirty_from = None;
            cache.dirty_through = None;
        }

        cache.trim_lines(&batch.visible);
        self.evict_old_documents(&batch.file_path);
        HighlightApplyOutcome {
            accepted: true,
            visual_changes,
        }
    }

    fn evict_old_documents(&mut self, keep: &Path) {
        // At most sixteen O(1) counters, never a walk through cached rows on
        // cursor/typing frames. The current document's visible working set is
        // retained even when it alone exceeds the optional cache budget.
        let mut bytes: usize = self.documents.values().map(|cache| cache.line_bytes).sum();
        while self.documents.len() > MAX_CACHED_DOCUMENTS || bytes > MAX_CACHED_LINE_BYTES {
            let Some(path) = self
                .documents
                .iter()
                .filter(|(path, _)| path.as_path() != keep)
                .min_by_key(|(_, cache)| cache.last_used)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            if let Some(removed) = self.documents.remove(&path) { bytes -= removed.line_bytes; }
        }
    }

    pub(crate) fn document_state_converged(&self, file_path: &Path, version: u64) -> bool {
        self.documents
            .get(file_path)
            .is_some_and(|cache| cache.version == version && cache.dirty_from.is_none())
    }

    /// Cache-only lookup for an authoritative/current display entry.
    #[allow(dead_code)]
    pub(crate) fn highlighted_line_if_cached(
        &mut self,
        line: &str,
        line_idx: usize,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
    ) -> Option<Rc<HighlightSpans>> {
        let file_path = file_path?;
        let hash = Self::line_hash(line);
        if let Some(cache) = self.documents.get_mut(file_path) {
            if cache.render_context_matches(theme_key, language) {
                cache.last_used = self.use_counter;
                if let Some(entry) = cache.lines.get(&line_idx) {
                    if cache.line_is_current(line_idx, entry.display_version) && entry.hash == hash {
                        return Some(Rc::clone(&entry.spans));
                    }
                }
            }
        }
        None
    }

    /// Version-keyed renderer lookup. Avoiding a full-line hash keeps the UI
    /// path independent of line length; worker batches already validate both
    /// the document and entry versions.
    pub(crate) fn highlighted_line_by_index_if_cached(
        &mut self,
        line_idx: usize,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
        content_version: u64,
    ) -> Option<(Rc<HighlightSpans>, Rc<Vec<usize>>)> {
        let file_path = file_path?;
        let cache = self.documents.get_mut(file_path)?;
        if cache.version != content_version || !cache.render_context_matches(theme_key, language) {
            return None;
        }
        cache.last_used = self.use_counter;
        let entry = cache.lines.get(&line_idx)?;
        cache.line_is_current(line_idx, entry.display_version)
            .then(|| (Rc::clone(&entry.spans), Rc::clone(&entry.span_char_lengths)))
    }

    /// Comment positions share the syntax cache's document/context validation.
    /// Never parse source text on the render thread to recover a missing map.
    #[cfg(test)]
    pub(crate) fn comment_ranges_if_cached(
        &self,
        line_idx: usize,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
        content_version: u64,
    ) -> Option<Rc<Vec<(usize, usize)>>> {
        let cache = self.documents.get(file_path?)?;
        if cache.version != content_version || !cache.render_context_matches(theme_key, language) {
            return None;
        }
        let entry = cache.lines.get(&line_idx)?;
        cache.line_is_current(line_idx, entry.display_version).then(|| Rc::clone(&entry.comment_ranges))
    }

    /// Stale-while-revalidate renderer fallback. An unchanged line is promoted
    /// after its current text hash proves that the cached spans still match.
    /// For a same-line edit, the old style runs are projected onto the current
    /// text so the UI never flashes back to plain text while TextMate catches up.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reconcile_stale_line_for_render(
        &mut self,
        line: &str,
        line_idx: usize,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
        content_version: u64,
    ) -> Option<(Rc<HighlightSpans>, Rc<Vec<usize>>)> {
        self.render_line(line, line_idx, file_path, language, theme_key, content_version)
            .map(|line| (line.spans, line.lengths))
    }

    /// Resolve current or provisional spans once per logical row, including
    /// comment ranges. Warm frames need one path lookup and one line lookup.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_line(
        &mut self,
        line: &str,
        line_idx: usize,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
        content_version: u64,
    ) -> Option<RenderedLine> {
        let file_path = file_path?;
        let cache = self.documents.get_mut(file_path)?;
        if cache.version != content_version || !cache.render_context_matches(theme_key, language) {
            return None;
        }
        cache.last_used = self.use_counter;
        let can_project = cache.dirty_from.is_some_and(|start| line_idx >= start)
            && cache.dirty_through.is_some_and(|end| line_idx <= end);
        let unchanged = line_idx < cache.unchanged_prefix;
        let entry = cache.lines.get_mut(&line_idx)?;
        if unchanged || entry.display_version == content_version {
            return Some(entry.rendered());
        }

        let normalized = normalized_line_prefix(line);
        let hash = Self::line_hash(&normalized);
        if entry.hash != hash {
            if !can_project {
                return None;
            }
            let spans = project_stale_spans(&entry.spans, &normalized);
            entry.span_char_lengths = Rc::new(span_char_lengths(&spans));
            entry.char_len = entry.span_char_lengths.iter().sum();
            entry.spans = Rc::new(spans);
            // An edit can insert/remove a comment delimiter. Wait for the
            // worker's classification rather than apply stale ranges to code.
            entry.comment_ranges = Rc::default();
            entry.hash = hash;
            entry.authoritative = false;
        }
        entry.display_version = content_version;
        // Display-ready does not mean parser-authoritative. prepare_job keeps
        // scheduling this line until a real TextMate batch replaces it.
        Some(entry.rendered())
    }

    /// Lookup for diff rows whose bounded content and first-line detection hash
    /// was prepared with the diff cache. Rendering does no syntax detection.
    pub(crate) fn highlighted_fragment_by_hash_if_cached(
        &self,
        hash: u64,
        line_idx: usize,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
    ) -> Option<Rc<HighlightSpans>> {
        let file_path = file_path?;
        self.fragment_cache.get(file_path, theme_key, language, line_idx, hash)
    }

    /// Highlight one isolated fragment with TextMate. This is intentionally
    /// restricted to diff rows, which do not form one parseable document.
    pub(crate) fn highlight_line_static_pub(
        line: &str,
        file_path: Option<&Path>,
        language: Option<&str>,
        theme_key: &str,
        syntax_set: &SyntaxRegistry,
        theme_set: &ThemeSet,
    ) -> HighlightSpans {
        FragmentHighlighter::new(line, file_path, language, theme_key, syntax_set, theme_set)
            .highlight_line(line)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn insert_fragment_cached(
        &mut self,
        file_path: &Path,
        theme_key: &str,
        language: Option<&str>,
        line_idx: usize,
        hash: u64,
        spans: HighlightSpans,
    ) {
        self.fragment_cache.insert(file_path, theme_key, language, line_idx, hash, spans);
    }

    pub(crate) fn shared_sets(&self) -> (Arc<SyntaxRegistry>, Arc<ThemeSet>) {
        (Arc::clone(&self.syntax_set), Arc::clone(&self.theme_set))
    }

    pub(crate) fn registry_generation(&self) -> u64 {
        self.registry_generation
    }
}

#[cfg(test)]
fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

/// Use the same original VS Code asset as the workbench and imported themes.
fn install_bundled_themes(theme_set: &mut ThemeSet) {
    for theme in crate::app_theme_loader::load_builtin_themes() {
        if let Some(syntax) = theme.textmate_theme { theme_set.themes.insert(theme.syntax_theme, syntax); }
    }
}

#[cfg(test)]
#[path = "highlight_checkpoint_performance_tests.rs"]
mod checkpoint_performance_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn write_refresh_grammar(folder: &Path, name: &str) {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("Example.tmLanguage.json"), serde_json::json!({
        "name":name,"scopeName":"source.example","fileTypes":["example"],
        "patterns":[{"match":"magic","name":"keyword.control.example"}]
    }).to_string()).unwrap();
    }

    #[test]
    fn syntax_refresh_installs_a_cache_rebuilt_by_another_process() {
        let directory = tempfile::tempdir().unwrap();
        write_refresh_grammar(directory.path(), "First");
        crate::syntax_assets::rebuild_syntax_cache(directory.path()).unwrap();
        let mut highlighter = Highlighter::for_syntax_folder(directory.path());
        write_refresh_grammar(directory.path(), "Other");
        crate::syntax_assets::rebuild_syntax_cache(directory.path()).unwrap();
        let loaded = highlighter.syntax_asset_refresh(Vec::new()).run().unwrap()
            .expect("a fresh disk cache must replace the stale in-memory registry");
        highlighter.install_syntax_assets(loaded);
        assert!(highlighter.syntax_names().contains(&"Other".into()));
        assert!(!highlighter.syntax_names().contains(&"First".into()));
    }

    #[test]
    fn syntax_refresh_first_launch_keeps_built_grammars_when_an_override_is_invalid() {
        let directory = tempfile::tempdir().unwrap();
        let installed = directory.path().join("installed");
        let custom = directory.path().join("custom");
        write_refresh_grammar(&installed, "Installed");
        std::fs::create_dir_all(&custom).unwrap();
        std::fs::write(custom.join("Bad.tmLanguage.json"), "contexts: [").unwrap();
        let mut highlighter = Highlighter::for_syntax_folder(&installed);
        let loaded = highlighter.syntax_asset_refresh(vec![custom]).run().unwrap()
            .expect("the newly built installed registry must become usable on this launch");
        assert!(loaded.warning.is_some(), "report the broken override");
        highlighter.install_syntax_assets(loaded);
        assert!(highlighter.syntax_names().contains(&"Installed".into()));
    }

    #[test]
    fn syntax_refresh_removes_overrides_when_their_directory_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let installed = directory.path().join("installed");
        let custom = directory.path().join("custom");
        write_refresh_grammar(&installed, "Installed");
        write_refresh_grammar(&custom, "Custom");
        let mut highlighter = Highlighter::for_syntax_folder(&installed);
        let loaded = highlighter.syntax_asset_refresh(vec![custom.clone()]).run().unwrap().unwrap();
        highlighter.install_syntax_assets(loaded);
        assert!(highlighter.syntax_names().contains(&"Custom".into()));
        std::fs::remove_file(custom.join("Example.tmLanguage.json")).unwrap();
        std::fs::remove_dir(&custom).unwrap();
        let loaded = highlighter.syntax_asset_refresh(vec![custom]).run().unwrap()
            .expect("removing the last custom folder must restore the installed registry");
        highlighter.install_syntax_assets(loaded);
        assert!(!highlighter.syntax_names().contains(&"Custom".into()));
        assert!(highlighter.syntax_names().contains(&"Installed".into()));
    }

    #[test]
    fn line_hash_deterministic() {
        let h1 = Highlighter::line_hash("hello world");
        let h2 = Highlighter::line_hash("hello world");
        assert_eq!(h1, h2);
    }

    #[test]
    fn line_hash_different_inputs() {
        let h1 = Highlighter::line_hash("hello");
        let h2 = Highlighter::line_hash("world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn line_hash_empty() {
        let h = Highlighter::line_hash("");
        // FNV-1a offset basis
        assert_eq!(h, 0xcbf29ce484222325);
    }

    #[test]
    fn dark_plus_theme_uses_vscode_editor_colors() {
        let highlighter = Highlighter::new();
        let theme = highlighter.theme_set.themes.get("dark-plus").unwrap();
        assert_eq!(theme.background, rgb(30, 30, 30));
        assert_eq!(theme.foreground, rgb(212, 212, 212));
        assert_eq!(theme.scope_foreground(&["string"]),rgb(206,145,120));
        assert_eq!(theme.scope_foreground(&["comment"]),rgb(106,153,85));
    }

    #[test]
    fn late_theme_registration_does_not_wait_for_active_highlight_snapshots() {
        let mut highlighter = Highlighter::new();
        let (_, active_snapshot) = highlighter.shared_sets();

        highlighter.register_theme("late-theme", Theme::new("test","#000000","#ffffff",&[]).unwrap());
        let (_, updated_snapshot) = highlighter.shared_sets();

        assert!(!active_snapshot.themes.contains_key("late-theme"));
        assert!(updated_snapshot.themes.contains_key("late-theme"));
    }

    #[test]
    fn syntax_registry_swap_rejects_an_in_flight_old_batch() {
        let mut highlighter = Highlighter::new();
        let rope = Rope::from_str("let value = 1;\n");
        let path = Path::new("generation.rs");
        let job = highlighter
            .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
            .unwrap();
        let (syntax_set, theme_set) = highlighter.shared_sets();
        let initial = job.clone().run(&rope, &syntax_set, &theme_set, || false);
        assert!(highlighter.apply_batch(initial));
        let (before, _) = highlighter
            .highlighted_line_by_index_if_cached(0, Some(path), None, "dark-plus", 0)
            .unwrap();
        let old_batch = job.run(&rope, &syntax_set, &theme_set, || false);

        let loaded = LoadedSyntaxAssets {
            installed_folder: highlighter.installed_syntax_folder.clone(),
            base_syntax_set: Arc::clone(&highlighter.base_syntax_set),
            fingerprint: highlighter.installed_syntax_fingerprint,
            syntax_set: Arc::clone(&highlighter.syntax_set),
            folders: Vec::new(),
            cache_write_error: None,
            warning: None,
        };
        highlighter.install_syntax_assets(loaded);

        let (during, _) = highlighter
            .highlighted_line_by_index_if_cached(0, Some(path), None, "dark-plus", 0)
            .expect("registry refresh must not flash existing text back to plain colors");
        assert!(Rc::ptr_eq(&before, &during));
        let fresh = highlighter
            .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
            .unwrap();
        assert!(fresh.start_state.is_none());
        assert!(!highlighter.apply_batch(old_batch));
    }

    #[test]
    fn dark_plus_highlights_c_with_vscode_token_roles() {
        let highlighter = Highlighter::new();
        let spans = Highlighter::highlight_line_static_pub(
            "int main(void) { return 42; } // note",
            Some(Path::new("sample.c")),
            None,
            "dark-plus",
            &highlighter.syntax_set,
            &highlighter.theme_set,
        );
        let color_for = |needle: &str| {
            spans
                .iter()
                .find(|(_, text)| text.trim() == needle)
                .map(|(style, _)| style.foreground)
        };

        assert_eq!(color_for("int"), Some(rgb(86, 156, 214)));
        assert_eq!(color_for("main"), Some(rgb(220, 220, 170)));
        assert_eq!(color_for("42"), Some(rgb(181, 206, 168)));
        assert_eq!(
            spans
                .iter()
                .find(|(_, text)| text.contains("note"))
                .map(|(style, _)| style.foreground),
            Some(rgb(106, 153, 85))
        );
    }

    #[test]
    fn dark_plus_highlights_python_tokens() {
        let highlighter = Highlighter::new();
        let spans = Highlighter::highlight_line_static_pub(
            "def greet(name): return f\"Hello, {name}\" # note",
            Some(Path::new("sample.py")),
            None,
            "dark-plus",
            &highlighter.syntax_set,
            &highlighter.theme_set,
        );
        assert!(spans.iter().any(|(style, text)| {
            text.contains("def") && style.foreground != rgb(212, 212, 212)
        }));
    }

    #[test]
    fn official_base_syntaxes_are_detected_and_colored() {
        let highlighter = Highlighter::new();
        let cases = [
            (
                "profile.ps1",
                "PowerShell",
                "$items = Get-ChildItem # files",
            ),
            (
                "component.tsx",
                "TSX",
                "const view = <Button disabled={false}>Save</Button>;",
            ),
            ("module.ts", "TypeScript", "const answer: number = 42;"),
            ("Cargo.toml", "TOML", "enabled = true # setting"),
            ("Dockerfile", "Dockerfile", "FROM rust:1.80 AS builder"),
            ("settings.ini", "INI", "enabled = true"),
            (
                "CMakeLists.txt",
                "CMake",
                "add_executable(vtcode src/main.cpp)",
            ),
            (".gitignore", "Ignore", "# generated files"),
            (
                "main.tf",
                "Terraform",
                "resource \"aws_s3_bucket\" \"main\" {}",
            ),
            ("schema.graphql", "GraphQL", "type Query { viewer: User! }"),
            ("counter.vhd", "VHDL", "entity counter is"),
            ("counter.vhdl", "VHDL", "architecture rtl of counter is"),
            ("counter.vho", "VHDL", "entity counter is"),
            ("counter.vht", "VHDL", "entity counter is"),
            ("counter.v", "Verilog", "module counter(input clk);"),
            ("counter.V", "Verilog", "module counter(input clk);"),
            ("counter.vh", "SystemVerilog", "parameter WIDTH = 8;"),
            (
                "counter.sv",
                "SystemVerilog",
                "always_ff @(posedge clk) q <= d;",
            ),
            ("counter.svh", "SystemVerilog", "typedef logic [7:0] byte_t;"),
            (
                "message.proto",
                "Protocol Buffer",
                "message User { string name = 1; }",
            ),
        ];

        for (file_name, expected_name, line) in cases {
            let path = Path::new(file_name);
            let syntax = select_syntax(&highlighter.syntax_set, Some(path), None, line);
            assert_eq!(syntax.name, expected_name, "wrong syntax for {file_name}");

            let spans = Highlighter::highlight_line_static_pub(
                line,
                Some(path),
                None,
                "dark-plus",
                &highlighter.syntax_set,
                &highlighter.theme_set,
            );
            assert!(
                spans.iter().any(|(style, text)| {
                    !text.trim().is_empty() && style.foreground != rgb(212, 212, 212)
                }),
                "{expected_name} detected {file_name} but did not color representative input"
            );
        }
    }

    fn run_job_to_completion(highlighter: &mut Highlighter, job: HighlightJob, rope: &Rope) {
        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = job.run_with_budget(
            rope,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert!(highlighter.apply_batch(batch));
    }

    #[test]
    fn pathological_lines_are_bounded_in_detection_and_worker_spans() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("minified.js");
        let source = format!(
            "{}TAIL\nconst ordinary = 1;\n",
            "x".repeat(MAX_SYNTAX_LINE_CHARS * 4)
        );
        let rope = Rope::from_str(&source);
        let job = highlighter
            .prepare_job(&rope, path, 0, None, 0..2, None, "dark-plus")
            .unwrap();
        assert!(job.first_line.chars().count() <= MAX_SYNTAX_DETECTION_CHARS + 1);

        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = job.run_with_budget(
            &rope,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        let first = batch
            .entries
            .iter()
            .find(|entry| entry.line == 0)
            .expect("visible minified line");
        assert_eq!(
            first.span_char_lengths.iter().sum::<usize>(),
            MAX_SYNTAX_LINE_CHARS
        );
        assert!(batch.entries.iter().any(|entry| entry.line == 1));
    }

    #[test]
    fn document_pipeline_preserves_multiline_textmate_state() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("multiline.c");
        let rope = Rope::from_str("int before;\n/* open\nstill a comment\n*/\nint after;\n");
        let job = highlighter
            .prepare_job(&rope, path, 0, None, 0..rope.len_lines(), None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, job, &rope);

        let spans = highlighter
            .highlighted_line_if_cached("still a comment\n", 2, Some(path), None, "dark-plus")
            .unwrap();
        assert!(spans.iter().any(|(style, text)| {
            text.contains("still a comment") && style.foreground == rgb(106, 153, 85)
        }));
    }

    #[test]
    fn worker_refresh_reuses_display_snapshots_and_replaces_only_changed_components() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("refresh.c");
        let mut rope = Rope::from_str("int first;\nint value; // comment 界\nint last;\n");
        let job = highlighter.prepare_job(&rope, path, 0, None, 0..3, None, "dark-plus").unwrap();
        let (syntaxes, themes) = highlighter.shared_sets();
        let batch = job.run(&rope, &syntaxes, &themes, || false);
        assert!(highlighter.apply_batch(batch.clone()));
        let before = highlighter.render_line("", 1, Some(path), None, "dark-plus", 0).unwrap();
        let outcome = highlighter.apply_batch_with_outcome(batch.clone());
        assert!(outcome.accepted() && !outcome.visual_changes());
        let repeated = highlighter.render_line("", 1, Some(path), None, "dark-plus", 0).unwrap();
        assert!(Rc::ptr_eq(&before.spans, &repeated.spans));
        assert!(Rc::ptr_eq(&before.lengths, &repeated.lengths));
        assert!(Rc::ptr_eq(&before.comments, &repeated.comments));

        let mut comments_only = batch.clone();
        comments_only.entries.iter_mut().find(|entry| entry.line == 1).unwrap().comment_ranges.clear();
        assert!(highlighter.apply_batch_with_outcome(comments_only).visual_changes());
        let comments = highlighter.render_line("", 1, Some(path), None, "dark-plus", 0).unwrap();
        assert!(Rc::ptr_eq(&before.spans, &comments.spans));
        assert!(comments.comments.is_empty() && !Rc::ptr_eq(&before.comments, &comments.comments));

        let mut recolored = batch.clone();
        recolored.entries.iter_mut().find(|entry| entry.line == 1).unwrap().spans[0].0.foreground.r ^= 1;
        assert!(highlighter.apply_batch_with_outcome(recolored).visual_changes());
        let colored = highlighter.render_line("", 1, Some(path), None, "dark-plus", 0).unwrap();
        assert!(!Rc::ptr_eq(&before.spans, &colored.spans));
        assert_eq!(colored.char_len, rope.line(1).to_string().trim_end_matches(['\r', '\n']).chars().count());

        // A real edit elsewhere advances parser authority but does not change
        // the neighboring row's display snapshot after it is restored.
        assert!(highlighter.apply_batch(batch));
        let before = highlighter.render_line("", 1, Some(path), None, "dark-plus", 0).unwrap();
        rope.insert(0, "unsigned ");
        let job = highlighter.prepare_job(&rope, path, 1, Some((0, 0)), 0..3, None, "dark-plus").unwrap();
        run_job_to_completion(&mut highlighter, job, &rope);
        let edited = highlighter.render_line("", 1, Some(path), None, "dark-plus", 1).unwrap();
        assert!(Rc::ptr_eq(&before.spans, &edited.spans));
        assert!(highlighter.documents[path].lines[&1].authoritative);
    }

    #[test]
    fn comment_ranges_follow_multiline_scopes_and_character_columns() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("comments.rs");
        let source = "let α = \"/* a b */\"; /* outer\n * a /* nested */ b\n */ let z = 2;\n";
        let rope = Rope::from_str(source);
        let job = highlighter.prepare_job(&rope, path, 0, None, 0..rope.len_lines(), None, "dark-plus").unwrap();
        run_job_to_completion(&mut highlighter, job, &rope);
        let ranges = |line| highlighter.comment_ranges_if_cached(line, Some(path), None, "dark-plus", 0).unwrap();
        let first = source.lines().next().unwrap();
        assert_eq!(ranges(0).as_slice(), &[(first[..first.rfind("/*").unwrap()].chars().count(), first.chars().count() + 1)]);
        assert_eq!(ranges(1).as_slice(), &[(0, rope.line(1).len_chars())]);
        assert_eq!(ranges(2).as_slice(), &[(0, 3)]);
        assert!(highlighter.comment_ranges_if_cached(0, Some(path), None, "dark-plus", 1).is_none());
        assert!(highlighter.comment_ranges_if_cached(0, Some(path), Some("Python"), "dark-plus", 0).is_none());
    }

    #[test]
    fn comment_ranges_are_revalidated_after_delimiter_edits() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("comment-edit.c");
        let original = Rope::from_str("/* a b */\n");
        let job = highlighter.prepare_job(&original, path, 0, None, 0..1, None, "dark-plus").unwrap();
        run_job_to_completion(&mut highlighter, job, &original);
        let edited = Rope::from_str("\"/* a b */\"\n");
        let job = highlighter.prepare_job(&edited, path, 1, Some((0, 0)), 0..1, None, "dark-plus").unwrap();
        highlighter.reconcile_stale_line_for_render(&edited.to_string(), 0, Some(path), None, "dark-plus", 1).unwrap();
        assert!(highlighter.comment_ranges_if_cached(0, Some(path), None, "dark-plus", 1).unwrap().is_empty());
        run_job_to_completion(&mut highlighter, job, &edited);
        assert!(highlighter.comment_ranges_if_cached(0, Some(path), None, "dark-plus", 1).unwrap().is_empty());
    }

    #[test]
    fn rendering_stale_suffix_does_not_prevent_parser_convergence() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("render-convergence.rs");
        let mut rope = Rope::from_str(&"let value = 1;\n".repeat(700));
        let job = highlighter
            .prepare_job(&rope, path, 0, None, 0..700, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, job, &rope);
        rope.insert(4, "new_");
        let job = highlighter
            .prepare_job(&rope, path, 1, Some((0, 0)), 0..400, None, "dark-plus")
            .unwrap();
        let (before, _) = highlighter
            .reconcile_stale_line_for_render(
                &rope.line(300).to_string(),
                300,
                Some(path),
                None,
                "dark-plus",
                1,
            )
            .unwrap();
        assert!(
            highlighter
                .prepare_job(&rope, path, 1, None, 300..301, None, "dark-plus")
                .is_some(),
            "displaying stale spans must not bypass parser validation"
        );
        run_job_to_completion(&mut highlighter, job, &rope);
        assert!(highlighter.document_state_converged(path, 1));
        assert!(
            highlighter
                .prepare_job(&rope, path, 1, None, 0..400, None, "dark-plus")
                .is_none(),
            "rendering must not cause endless suffix rehighlighting"
        );
        let (after, _) = highlighter
            .highlighted_line_by_index_if_cached(300, Some(path), None, "dark-plus", 1)
            .unwrap();
        assert!(Rc::ptr_eq(&before, &after));
    }

    #[test]
    fn later_newline_shifts_only_its_own_suffix_during_pending_propagation() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("rapid-newline.rs");
        let source: String = (0..100)
            .map(|i| format!("let value_{i} = {i};\n"))
            .collect();
        let mut rope = Rope::from_str(&source);
        let job = highlighter
            .prepare_job(&rope, path, 0, None, 0..100, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, job, &rope);
        rope.insert(4, "new_");
        highlighter.prepare_job(&rope, path, 1, Some((0, 0)), 0..100, None, "dark-plus");
        rope.insert(rope.line_to_char(20), "// inserted\n");
        highlighter.prepare_job(&rope, path, 2, Some((20, 21)), 0..101, None, "dark-plus");
        for line in [5, 19, 31, 99] {
            let text = rope.line(line).to_string();
            let (spans, _) = highlighter
                .reconcile_stale_line_for_render(&text, line, Some(path), None, "dark-plus", 2)
                .expect("unchanged rows must remain colored during rapid newline edits");
            assert_eq!(
                spans
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<String>(),
                text
            );
        }
    }

    #[test]
    fn worker_returns_viewport_without_parsing_an_unrequested_suffix() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("viewport-budget.rs");
        let rope = Rope::from_str(&"let value = 1;\n".repeat(20_000));
        let job = highlighter
            .prepare_job(&rope, path, 0, None, 0..75, None, "dark-plus")
            .unwrap();
        let (syntaxes, themes) = highlighter.shared_sets();
        let batch = job.run_with_budget(
            &rope,
            &syntaxes,
            &themes,
            &mut || false,
            Duration::from_secs(10),
        );
        assert_eq!(batch.parsed_to, 75);
        assert_eq!(batch.entries.len(), 75);
    }

    #[test]
    fn unchanged_lines_keep_cached_colors_while_an_edit_rehighlights() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("no-flicker.rs");
        let rope = Rope::from_str("let first = 1;\nlet second = 2;\n");
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 0..2, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);
        let (before, before_lengths) = highlighter
            .highlighted_line_by_index_if_cached(1, Some(path), None, "dark-plus", 0)
            .unwrap();

        let mut edited = rope.clone();
        edited.insert(edited.line_to_char(0) + 4, "renamed_");
        let pending = highlighter
            .prepare_job(&edited, path, 1, Some((0, 0)), 0..2, None, "dark-plus")
            .unwrap();
        drop(pending);
        assert!(
            highlighter
                .highlighted_line_by_index_if_cached(1, Some(path), None, "dark-plus", 1)
                .is_none()
        );

        let current_line = edited.line(1).to_string();
        let (during, during_lengths) = highlighter
            .reconcile_stale_line_for_render(&current_line, 1, Some(path), None, "dark-plus", 1)
            .unwrap();
        assert_eq!(during.as_ref(), before.as_ref());
        assert_eq!(during_lengths.as_ref(), before_lengths.as_ref());
        assert!(
            highlighter
                .highlighted_line_by_index_if_cached(1, Some(path), None, "dark-plus", 1)
                .is_some()
        );
        assert!(!highlighter.document_state_converged(path, 1));
    }

    #[test]
    fn undoing_a_distant_newline_keeps_prefix_colors_exactly_cached() {
        let path = Path::new("undo-no-flicker.rs");
        let source: String = (0..400)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect();
        let mut editor = crate::editor::Editor::test_with_text(&source);
        editor.file_path = Some(path.to_path_buf());
        let mut highlighter = Highlighter::new();
        let initial = highlighter
            .prepare_job(
                &editor.rope,
                path,
                editor.content_version,
                None,
                0..25,
                None,
                "dark-plus",
            )
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &editor.rope);

        editor.cursor_line = 300;
        editor.cursor_col = 0;
        editor.insert_newline();
        editor.mark_syntax_dirty_range(300, 301);
        let _ = highlighter.prepare_job(
            &editor.rope,
            path,
            editor.content_version,
            Some((300, 301)),
            0..25,
            None,
            "dark-plus",
        );

        editor.undo();
        let changed = (
            editor.syntax_dirty_from.unwrap(),
            editor.syntax_dirty_through.unwrap(),
        );
        let _ = highlighter.prepare_job(
            &editor.rope,
            path,
            editor.content_version,
            Some(changed),
            0..25,
            None,
            "dark-plus",
        );

        assert!(
            highlighter
                .highlighted_line_by_index_if_cached(
                    10,
                    Some(path),
                    None,
                    "dark-plus",
                    editor.content_version,
                )
                .is_some()
        );
    }

    #[test]
    fn edited_line_keeps_styles_without_rendering_stale_text() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("projected-no-flicker.rs");
        let rope = Rope::from_str("let value = 1;\n");
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 0..1, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);

        let mut edited = rope.clone();
        edited.insert(edited.line_to_char(0) + 4, "renamed_");
        let pending = highlighter
            .prepare_job(&edited, path, 1, Some((0, 0)), 0..1, None, "dark-plus")
            .unwrap();
        drop(pending);

        let current_line = edited.line(0).to_string();
        let (projected, lengths) = highlighter
            .reconcile_stale_line_for_render(&current_line, 0, Some(path), None, "dark-plus", 1)
            .unwrap();
        let rendered: String = projected.iter().map(|(_, text)| text.as_str()).collect();
        let mut expected = current_line;
        Highlighter::normalize_line_for_highlighting(&mut expected);
        assert_eq!(rendered, expected);
        assert_eq!(
            lengths.iter().sum::<usize>(),
            expected.trim_end_matches('\n').chars().count()
        );
        assert!(
            projected.len() > 1,
            "the projected line lost all syntax styles"
        );
        assert!(
            highlighter
                .prepare_job(&edited, path, 1, Some((0, 0)), 0..1, None, "dark-plus",)
                .is_some()
        );
    }

    #[test]
    fn inserted_newline_keeps_both_split_fragments_colored() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("split-no-flicker.rs");
        let rope = Rope::from_str("let answer = calculate(value);\nlet next = 2;\n");
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 0..2, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);

        let mut edited = rope.clone();
        edited.insert(edited.line_to_char(0) + 13, "\n");
        let pending = highlighter
            .prepare_job(&edited, path, 1, Some((0, 1)), 0..3, None, "dark-plus")
            .unwrap();
        drop(pending);

        for line_idx in 0..=1 {
            let current = edited.line(line_idx).to_string();
            let (spans, _) = highlighter
                .reconcile_stale_line_for_render(
                    &current,
                    line_idx,
                    Some(path),
                    None,
                    "dark-plus",
                    1,
                )
                .expect("both halves of the split line should retain display styles");
            assert_eq!(
                spans
                    .iter()
                    .map(|(_, text)| text.as_str())
                    .collect::<String>(),
                normalized_line_prefix(&current),
                "stale styling must never render stale characters"
            );
        }
    }

    #[test]
    fn undoing_a_blank_line_insertion_restores_colors_before_worker_finishes() {
        let path = Path::new("undo-blank.rs");
        let original = Rope::from_str("let answer = calculate(42);\nlet next = 2;\n");
        let mut highlighter = Highlighter::new();
        let job = highlighter
            .prepare_job(&original, path, 0, None, 0..2, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, job, &original);
        let before = highlighter
            .highlighted_line_by_index_if_cached(0, Some(path), None, "dark-plus", 0)
            .unwrap()
            .0;

        let mut edited = original.clone();
        edited.insert(0, "\n");
        let job = highlighter
            .prepare_job(&edited, path, 1, Some((0, 1)), 0..3, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, job, &edited);

        let pending = highlighter
            .prepare_job(&original, path, 2, Some((0, 1)), 0..2, None, "dark-plus")
            .unwrap();
        drop(pending);
        let after = highlighter
            .reconcile_stale_line_for_render(
                &original.line(0).to_string(),
                0,
                Some(path),
                None,
                "dark-plus",
                2,
            )
            .unwrap()
            .0;
        assert_eq!(after.as_ref(), before.as_ref());
    }

    #[test]
    fn deleted_newline_keeps_the_joined_line_colored() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("join-no-flicker.rs");
        let rope = Rope::from_str("let left = 1;\nlet right = 2;\n");
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 0..2, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);

        let mut edited = rope.clone();
        let newline = edited.line_to_char(1) - 1;
        edited.remove(newline..newline + 1);
        let pending = highlighter
            .prepare_job(&edited, path, 1, Some((0, 0)), 0..1, None, "dark-plus")
            .unwrap();
        drop(pending);

        let current = edited.line(0).to_string();
        let (spans, _) = highlighter
            .reconcile_stale_line_for_render(&current, 0, Some(path), None, "dark-plus", 1)
            .expect("the joined line should retain display styles");
        assert_eq!(
            spans
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<String>(),
            normalized_line_prefix(&current)
        );
        assert!(spans.len() > 1, "the joined line lost all syntax styles");
    }

    #[test]
    fn changing_a_shebang_resets_the_selected_syntax_state() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("script");
        let python = Rope::from_str("#!/usr/bin/env python3\nprint('hello')\n");
        let initial = highlighter
            .prepare_job(
                &python,
                path,
                0,
                None,
                0..python.len_lines(),
                None,
                "dark-plus",
            )
            .unwrap();
        let python_syntax = initial.syntax_name.clone();
        run_job_to_completion(&mut highlighter, initial, &python);

        let shell = Rope::from_str("#!/bin/bash\necho hello\n");
        let changed = highlighter
            .prepare_job(
                &shell,
                path,
                1,
                Some((0, 0)),
                0..shell.len_lines(),
                None,
                "dark-plus",
            )
            .unwrap();

        assert_ne!(changed.syntax_name, python_syntax);
        assert_eq!(changed.parse_start, 0);
        assert!(changed.start_state.is_none());
    }

    #[test]
    fn line_insertion_restarts_at_checkpoint_and_reuses_converged_suffix() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("large.rs");
        let source: String = (0..700)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect();
        let rope = Rope::from_str(&source);
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 0..rope.len_lines(), None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);

        let mut edited = rope.clone();
        let insertion = edited.line_to_char(300);
        edited.insert(insertion, "// one inserted line\n");
        let incremental = highlighter
            .prepare_job(
                &edited,
                path,
                1,
                Some((300, 301)),
                290..540,
                None,
                "dark-plus",
            )
            .unwrap();
        assert_eq!(incremental.parse_start, 256);

        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = incremental.run_with_budget(
            &edited,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert_eq!(batch.converged_at, Some(513));
        assert!(batch.parsed_to < edited.len_lines());
        assert!(highlighter.apply_batch(batch));

        let mut shifted_line = edited.line(651).to_string();
        Highlighter::normalize_line_for_highlighting(&mut shifted_line);
        assert!(
            highlighter
                .highlighted_line_if_cached(&shifted_line, 651, Some(path), None, "dark-plus",)
                .is_some()
        );
    }

    #[test]
    fn line_deletion_reuses_a_shifted_suffix_with_modern_textmate_states() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("large.rs");
        let source: String = (0..700)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect();
        let rope = Rope::from_str(&source);
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 0..rope.len_lines(), None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);

        let mut edited = rope.clone();
        let removed = edited.line_to_char(300)..edited.line_to_char(301);
        edited.remove(removed);
        let incremental = highlighter
            .prepare_job(
                &edited,
                path,
                1,
                Some((300, 300)),
                290..540,
                None,
                "dark-plus",
            )
            .unwrap();
        assert_eq!(incremental.parse_start, 256);

        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = incremental.run_with_budget(
            &edited,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert_eq!(batch.converged_at, Some(511));
        assert!(batch.parsed_to < edited.len_lines());
        assert!(highlighter.apply_batch(batch));
    }

    #[test]
    fn consecutive_line_insertions_preserve_shifted_checkpoint_offsets() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("large.rs");
        let source: String = (0..900)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect();
        let rope = Rope::from_str(&source);
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 0..rope.len_lines(), None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);

        let mut first_edit = rope.clone();
        let first_insertion = first_edit.line_to_char(300);
        first_edit.insert(first_insertion, "// first inserted line\n");
        let first_job = highlighter
            .prepare_job(
                &first_edit,
                path,
                1,
                Some((300, 301)),
                290..540,
                None,
                "dark-plus",
            )
            .unwrap();
        let (syntax_set, theme_set) = highlighter.shared_sets();
        let first_batch = first_job.run_with_budget(
            &first_edit,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert_eq!(first_batch.converged_at, Some(513));
        assert!(highlighter.apply_batch(first_batch));

        let mut second_edit = first_edit.clone();
        let second_insertion = second_edit.line_to_char(600);
        second_edit.insert(second_insertion, "// second inserted line\n");
        let second_job = highlighter
            .prepare_job(
                &second_edit,
                path,
                2,
                Some((600, 601)),
                590..800,
                None,
                "dark-plus",
            )
            .unwrap();
        let second_batch = second_job.run_with_budget(
            &second_edit,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert_eq!(second_batch.converged_at, Some(770));
        assert!(second_batch.parsed_to < second_edit.len_lines());
        assert!(highlighter.apply_batch(second_batch));
    }

    #[test]
    fn rapid_insertion_cannot_converge_before_an_earlier_pending_edit() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("rapid.rs");
        let rope = Rope::from_str(&"let value = 1;\n".repeat(900));
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 700..710, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);

        let mut first_edit = rope.clone();
        let line_start = first_edit.line_to_char(700);
        let line_end = first_edit.line_to_char(701);
        first_edit.remove(line_start..line_end);
        first_edit.insert(line_start, "/* open comment\n");
        let pending = highlighter
            .prepare_job(
                &first_edit,
                path,
                1,
                Some((700, 700)),
                690..720,
                None,
                "dark-plus",
            )
            .unwrap();
        drop(pending);

        let mut second_edit = first_edit.clone();
        second_edit.insert(second_edit.line_to_char(100), "// inserted first\n");
        let incremental = highlighter
            .prepare_job(
                &second_edit,
                path,
                2,
                Some((100, 700)),
                690..720,
                None,
                "dark-plus",
            )
            .unwrap();
        assert_eq!(incremental.invalidated_through, Some(701));

        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = incremental.run_with_budget(
            &second_edit,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert_ne!(batch.converged_at, Some(701));
        assert!(batch.parsed_to > 701);
    }

    #[test]
    fn distant_viewport_is_processed_in_resumable_bounded_batches() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("very-large.rs");
        let rope = Rope::from_str(&"let value = 1;\n".repeat(20_000));
        let job = highlighter
            .prepare_job(&rope, path, 0, None, 15_000..15_050, None, "dark-plus")
            .unwrap();
        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = job.run_with_budget(
            &rope,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert_eq!(batch.parsed_to, MAX_LINES_PER_BATCH);
        assert!(batch.entries.is_empty());
        assert!(highlighter.apply_batch(batch));

        let resumed = highlighter
            .prepare_job(&rope, path, 0, None, 15_000..15_050, None, "dark-plus")
            .unwrap();
        assert_eq!(resumed.parse_start, MAX_LINES_PER_BATCH);
    }

    #[test]
    fn authoritative_visible_rows_are_published_as_one_atomic_batch() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("atomic-visible.rs");
        let rope = Rope::from_str(&"let value = 1;\n".repeat(100));
        let prefix = highlighter
            .prepare_job(&rope, path, 0, None, 10..11, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, prefix, &rope);

        let visible = highlighter
            .prepare_job(&rope, path, 0, None, 10..30, None, "dark-plus")
            .unwrap();
        assert_eq!(visible.parse_start, 11);
        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = visible.run_with_budget(
            &rope,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::ZERO,
        );

        assert_eq!(batch.parsed_to, 30);
        assert_eq!(batch.entries.len(), 19);
    }

    #[test]
    fn distant_viewport_gets_provisional_textmate_spans_without_prefix_scan() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("very-large.cpp");
        let rope = Rope::from_str(
            &"int value = std::vector<int>{1, 2}.size(); // comment\n".repeat(20_000),
        );
        let exact = highlighter
            .prepare_job(&rope, path, 0, None, 15_000..15_050, None, "dark-plus")
            .unwrap();
        let provisional = highlighter.prepare_provisional_job(&exact).unwrap();
        assert!(provisional.provisional);
        assert_eq!(provisional.parse_start, 15_000);

        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = provisional.run(&rope, &syntax_set, &theme_set, || false);
        assert!(batch.is_provisional());
        assert_eq!(batch.parsed_to, 15_050);
        assert_eq!(batch.entries.len(), 50);
        assert!(highlighter.apply_batch(batch));

        let line = rope.line(15_000).to_string();
        assert!(
            highlighter
                .highlighted_line_if_cached(&line, 15_000, Some(path), None, "dark-plus")
                .is_some()
        );

        // Provisional display data must not masquerade as a parser checkpoint.
        let still_exact = highlighter
            .prepare_job(&rope, path, 0, None, 15_000..15_050, None, "dark-plus")
            .unwrap();
        assert!(!still_exact.provisional);
        assert_eq!(still_exact.parse_start, 0);
        // Nor should the same viewport enqueue duplicate provisional work.
        assert!(highlighter.prepare_provisional_job(&still_exact).is_none());
    }

    #[test]
    fn provisional_pass_does_not_recolor_matching_stale_lines() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("shifted-comment.c");
        let source = format!("/* open comment\n{}", "still a comment\n".repeat(700));
        let rope = Rope::from_str(&source);
        let initial = highlighter
            .prepare_job(&rope, path, 0, None, 300..306, None, "dark-plus")
            .unwrap();
        run_job_to_completion(&mut highlighter, initial, &rope);
        let (before, _) = highlighter
            .highlighted_line_by_index_if_cached(300, Some(path), None, "dark-plus", 0)
            .unwrap();

        let mut edited = rope.clone();
        edited.insert(edited.line_to_char(10), "inserted comment line\n");
        let authoritative = highlighter
            .prepare_job(
                &edited,
                path,
                1,
                Some((10, 11)),
                301..307,
                None,
                "dark-plus",
            )
            .unwrap();
        let provisional = highlighter
            .prepare_provisional_job(&authoritative)
            .expect("distant viewport should receive a provisional pass");
        let (syntax_set, theme_set) = highlighter.shared_sets();
        let batch = provisional.run(&edited, &syntax_set, &theme_set, || false);
        let outcome = highlighter.apply_batch_with_outcome(batch);
        assert!(outcome.accepted());
        assert!(
            !outcome.visual_changes(),
            "matching stale spans should not manufacture a repaint"
        );

        let (during, _) = highlighter
            .highlighted_line_by_index_if_cached(301, Some(path), None, "dark-plus", 1)
            .unwrap();
        assert_eq!(
            during.as_ref(),
            before.as_ref(),
            "a context-free provisional parse must not replace matching stale display spans"
        );
    }

    #[test]
    fn late_provisional_batch_cannot_replace_authoritative_spans() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("race.cpp");
        let rope = Rope::from_str(&"int value = 1;\n".repeat(400));
        let exact = highlighter
            .prepare_job(&rope, path, 0, None, 300..320, None, "dark-plus")
            .unwrap();
        let provisional = highlighter.prepare_provisional_job(&exact).unwrap();
        let (syntax_set, theme_set) = highlighter.shared_sets();
        let late_batch = provisional.run(&rope, &syntax_set, &theme_set, || false);

        let exact_batch = exact.run_with_budget(
            &rope,
            &syntax_set,
            &theme_set,
            &mut || false,
            Duration::from_secs(10),
        );
        assert!(highlighter.apply_batch(exact_batch));
        assert!(highlighter.documents[path].lines[&300].authoritative);

        assert!(highlighter.apply_batch(late_batch));
        assert!(highlighter.documents[path].lines[&300].authoritative);
    }

    #[test]
    fn nearby_checkpoint_does_not_spawn_provisional_work() {
        let mut highlighter = Highlighter::new();
        let path = Path::new("nearby.cpp");
        let rope = Rope::from_str(&"int value = 1;\n".repeat(400));
        let exact = highlighter
            .prepare_job(&rope, path, 0, None, 200..220, None, "dark-plus")
            .unwrap();
        assert!(highlighter.prepare_provisional_job(&exact).is_none());
    }

    #[test]
    fn syntax_names_expose_every_visible_installed_language() {
        let highlighter = Highlighter::new();
        let names = highlighter.syntax_names();

        assert!(names.len() >= 240);
        assert!(names.iter().any(|name| name == "VHDL"));
        assert!(names.iter().any(|name| name == "Verilog"));
        assert!(names.iter().any(|name| name == "SystemVerilog"));
        assert!(names.iter().any(|name| name == "Rust"));
        assert!(names.iter().any(|name| name == "PowerShell"));
        assert!(names.iter().any(|name| name == "TypeScript"));
        assert!(names.iter().any(|name| name == "TSX"));
        assert!(names.iter().any(|name| name == "TOML"));
        assert!(names.iter().any(|name| name == "YAML"));
        assert!(names.iter().any(|name| name == "ERB"));
        assert!(
            names
                .windows(2)
                .all(|pair| { pair[0].to_ascii_lowercase() <= pair[1].to_ascii_lowercase() })
        );
        assert!(
            highlighter
                .syntax_set
                .syntaxes()
                .iter()
                .filter(|syntax| syntax.hidden)
                .all(|syntax| !names.contains(&syntax.name))
        );
    }

    #[test]
    fn loads_textmate_grammars_on_top_of_built_ins() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("Foo.tmLanguage.json"),
            r#"{"name":"Foo Language","scopeName":"source.foo","fileTypes":["foo","rs"],"patterns":[{"match":"\\bmagic\\b","name":"keyword.control.foo"}]}"#,
        )
        .unwrap();

        let mut highlighter = Highlighter::new();
        let refresh = SyntaxAssetRefresh {
            bootstrap: false,
            installed_folder: None,
            force_install: false,
            loaded_fingerprint: None,
            base_syntax_set: Arc::clone(&highlighter.base_syntax_set),
            candidates: vec![directory.path().to_path_buf()],
            previous_folders: Vec::new(),
        };
        let loaded = refresh
            .run()
            .unwrap()
            .expect("custom syntax folder should produce a registry update");
        highlighter.install_syntax_assets(loaded);
        assert!(
            highlighter
                .syntax_names()
                .iter()
                .any(|name| name == "Foo Language")
        );
        assert_eq!(
            highlighter
                .syntax_set
                .find_syntax_by_extension("foo")
                .unwrap()
                .name,
            "Foo Language"
        );
        assert!(highlighter.syntax_set.find_syntax_by_name("Rust").is_some());
        assert_eq!(
            highlighter
                .syntax_set
                .find_syntax_by_extension("rs")
                .unwrap()
                .name,
            "Foo Language"
        );
        let associated = Rope::from_str("magic\n");
        let associated_job = highlighter
            .prepare_job(
                &associated,
                Path::new("sample.unknown"),
                0,
                None,
                0..1,
                Some("Foo Language"),
                "dark-plus",
            )
            .unwrap();
        assert_eq!(associated_job.syntax_name, "Foo Language");

        let spans = Highlighter::highlight_line_static_pub(
            "magic ordinary\n",
            Some(Path::new("sample.foo")),
            None,
            "dark-plus",
            &highlighter.syntax_set,
            &highlighter.theme_set,
        );
        assert!(
            spans
                .iter()
                .any(|(style, text)| { text == "magic" && style.foreground == rgb(197, 134, 192) })
        );
    }
}
