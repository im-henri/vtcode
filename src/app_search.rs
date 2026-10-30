use crate::app::{App, fuzzy_match};
use std::path::PathBuf;
#[cfg(test)]
use crate::source_symbols::source_symbol;

#[cfg(test)]
#[path = "source_symbol_performance_tests.rs"]
mod symbol_performance_tests;

#[cfg(test)]
#[path = "app_search_correctness_tests.rs"]
mod correctness_tests;

#[cfg(test)]
#[path = "app_workspace_replace_tests.rs"]
mod workspace_replace_tests;

const QUICK_OPEN_RESULT_LIMIT: usize = 20;
const QUICK_OPEN_MATCH_CACHE_LIMIT: usize = 65_536;
const QUICK_OPEN_NAME_SCORE_CACHE_LIMIT: usize = 4_096;
const QUICK_OPEN_NAME_SCORE_SAMPLE: usize = 128;
const QUICK_OPEN_NAME_SCORE_MIN_REUSE: usize = 16;

#[derive(Clone)]
pub(crate) struct QuickOpenIndex {
    files: std::sync::Arc<std::sync::Mutex<Option<QuickOpenFileIndex>>>,
    ranking: std::sync::Arc<std::sync::Mutex<QuickOpenRankingCache>>,
    service: Option<std::sync::Arc<std::sync::OnceLock<LatestQueryService>>>,
}

impl Default for QuickOpenIndex {
    fn default() -> Self {
        Self {
            files: Default::default(),
            ranking: Default::default(),
            service: Some(Default::default()),
        }
    }
}

pub(crate) struct LatestQueryJob {
    pub cancel: crate::background::CancellationToken,
    pub run: Box<dyn FnOnce() + Send>,
}

pub(crate) struct LatestQueryService {
    pending: std::sync::Arc<std::sync::Mutex<Option<LatestQueryJob>>>,
    wake: std::sync::mpsc::SyncSender<()>,
}

impl LatestQueryService {
    pub(crate) fn channel() -> (Self, impl FnOnce() + Send + 'static) {
        let pending = std::sync::Arc::new(std::sync::Mutex::new(None::<LatestQueryJob>));
        let (wake, receiver) = std::sync::mpsc::sync_channel(1);
        let worker_pending = std::sync::Arc::clone(&pending);
        let worker = move || {
            while receiver.recv().is_ok() {
                let job = worker_pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                if let Some(job) = job
                    && !job.cancel.is_cancelled()
                {
                    // A failed request must not terminate the shared service.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job.run));
                }
            }
        };
        (Self { pending, wake }, worker)
    }

    pub(crate) fn replace(&self, job: LatestQueryJob) {
        let obsolete = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .replace(job);
        // Cancellation and dropping an activity ticket happen outside the UI
        // slot lock. Traversal and ranking never hold this mutex either.
        if let Some(obsolete) = obsolete {
            obsolete.cancel.cancel();
        }
        let _ = self.wake.try_send(()); // One wake represents the latest slot.
    }
}

impl Drop for LatestQueryService {
    fn drop(&mut self) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(pending) = pending {
            pending.cancel.cancel();
        }
        // Dropping the sole sender disconnects recv after an active request.
    }
}

struct QuickOpenFileIndex {
    remote_version: Option<u64>,
    root: PathBuf,
    generation: u64,
    entries: std::sync::Arc<Vec<QuickOpenEntry>>,
    built_at: std::time::Instant,
    excludes: crate::settings::ExcludePatterns,
}

struct QuickOpenEntry {
    path: PathBuf,
    name: String,
    relative: String,
}

impl QuickOpenIndex {
    fn submit_latest(
        &self,
        background: &mut crate::background::BackgroundWorkers,
        run: impl FnOnce(Self, crate::background::CancellationToken) + Send + 'static,
    ) -> crate::background::CancellationToken {
        let service = self
            .service
            .as_ref()
            .expect("only a UI index submits Quick Open work")
            .get_or_init(|| {
                let (service, worker) = LatestQueryService::channel();
                background.spawn_untracked("quick-open-service", worker);
                service
            });
        let cancel = crate::background::CancellationToken::new();
        let token = cancel.clone();
        let activity = background
            .activity
            .start(crate::performance::worker_label("quick-open"), None)
            .cancellable(cancel.clone());
        // Requests retain only cache state. Capturing the original handle here
        // would keep the service sender alive from its own pending slot.
        let worker_index = Self {
            files: std::sync::Arc::clone(&self.files),
            ranking: std::sync::Arc::clone(&self.ranking),
            service: None,
        };
        service.replace(LatestQueryJob {
            cancel: token.clone(),
            run: Box::new(move || {
                let _activity = activity.running();
                run(worker_index, cancel);
            }),
        });
        token
    }

    #[cfg(test)]
    fn files(
        &self,
        root: &std::path::Path,
        generation: u64,
        watched: bool,
        cancel: &crate::background::CancellationToken,
    ) -> Option<std::sync::Arc<Vec<QuickOpenEntry>>> {
        self.files_with_remote(root, generation, watched, cancel, None,
            &crate::settings::default_files_exclude().into_iter().chain(crate::settings::default_search_exclude()).collect())
    }
    fn files_with_remote(
        &self,
        root: &std::path::Path,
        generation: u64,
        watched: bool,
        cancel: &crate::background::CancellationToken,
        remote: Option<&crate::remote::workspace::Workspace>,
        excludes: &crate::settings::ExcludePatterns,
    ) -> Option<std::sync::Arc<Vec<QuickOpenEntry>>> {
        let mut cached = self
            .files
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cancel.is_cancelled() {
            return None;
        }
        // Native watches on shared filesystems can miss another client's
        // changes. Bound reuse even when the backend reported a healthy watch.
        let max_age = std::time::Duration::from_secs(if watched { 30 } else { 2 });
        if let Some(index) = cached.as_ref().filter(|index| {
            index.root == root
                && index.excludes == *excludes
                && !excludes.values().any(serde_json::Value::is_object)
                && index.generation == generation
                && index.built_at.elapsed() < max_age
        }) {
            return Some(std::sync::Arc::clone(&index.entries));
        }
        if let Some(remote) = remote {
            let known_version = cached
                .as_ref()
                .filter(|index| index.root == root)
                .and_then(|index| index.remote_version);
            let crate::remote::Reply::Files { version, files } = remote
                .client
                .call(crate::remote::Command::Files {
                    generation,
                    known_version,
                    excludes: excludes.clone(),
                })
                .ok()?
            else {
                return None;
            };
            if let Some(files) = files {
                let entries = std::sync::Arc::new(
                    files
                        .into_iter()
                        .map(|path| QuickOpenEntry {
                            path: remote.view_key(&path),
                            name: path.name().to_lowercase(),
                            relative: path.display().to_lowercase(),
                        })
                        .collect(),
                );
                *cached = Some(QuickOpenFileIndex {
                    root: root.to_path_buf(),
                    generation,
                    entries,
                    built_at: std::time::Instant::now(),
                    remote_version: Some(version),
                    excludes: excludes.clone(),
                });
            } else if let Some(index) = cached.as_mut() {
                index.generation = generation;
                index.excludes = excludes.clone();
                index.built_at = std::time::Instant::now();
            }
            return cached.as_ref().map(|index| index.entries.clone());
        }
        // Finish a shared index even if this query is superseded. Subsequent
        // requests reuse the completed traversal rather than
        // repeatedly restarting it. The UI never acquires this mutex.
        let rules = crate::search_paths::ExcludeRules::new(excludes, &Default::default());
        let entries = walkdir::WalkDir::new(root)
            .min_depth(1)
            .into_iter()
            .filter_entry(|entry| {
                !rules.excludes(&crate::search_paths::Candidate::new(entry.path()), root)
                    && !crate::editor::is_save_temporary_path(entry.path())
            })
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                let name = entry.file_name().to_str().unwrap_or("").to_lowercase();
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(entry.path())
                    .to_string_lossy()
                    .to_lowercase()
                    .replace('\\', "/");
                QuickOpenEntry {
                    path: entry.into_path(),
                    name,
                    relative,
                }
            })
            .collect();
        let entries = std::sync::Arc::new(entries);
        *cached = Some(QuickOpenFileIndex {
            root: root.to_path_buf(),
            generation,
            entries: std::sync::Arc::clone(&entries),
            built_at: std::time::Instant::now(),
            remote_version: None,
            excludes: excludes.clone(),
        });
        Some(entries)
    }

    fn rank_files(
        &self,
        entries: &std::sync::Arc<Vec<QuickOpenEntry>>,
        query: &str,
        cancel: &crate::background::CancellationToken,
    ) -> Option<Vec<(i64, PathBuf)>> {
        // Ranking requests serialize only on workers. The UI merely clones
        // this index handle and never waits for either shared cache mutex.
        self.ranking
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .rank(entries, query, || cancel.is_cancelled())
    }
}

struct CompletedQuickOpenRanking {
    source: std::sync::Weak<Vec<QuickOpenEntry>>,
    query: String,
    path_query: bool,
    matches: Option<Vec<usize>>,
    results: Vec<(i64, PathBuf)>,
}

#[derive(Default)]
struct QuickOpenRankingCache {
    completed: Option<CompletedQuickOpenRanking>,
    spare: Vec<usize>,
    /// Comparative benchmarks disable only name memoization, keeping result
    /// caching, candidate retention and cancellation work identical.
    #[cfg(test)]
    force_direct_name_scores: bool,
}

/// Sample names while doing the requested scoring. Mostly unique workspaces
/// then avoid hash lookups for the rest of the query, without an upfront scan.
struct QuickOpenNameScores<'a> {
    scores: rustc_hash::FxHashMap<&'a str, Option<i64>>,
    sampled: usize,
    reused: usize,
    enabled: bool,
}

impl Default for QuickOpenNameScores<'_> {
    fn default() -> Self {
        Self {
            scores: Default::default(),
            sampled: 0,
            reused: 0,
            enabled: true,
        }
    }
}

impl<'a> QuickOpenNameScores<'a> {
    fn score(&mut self, name: &'a str, calculate: impl FnOnce() -> Option<i64>) -> Option<i64> {
        if !self.enabled {
            return calculate();
        }
        let reused = self.scores.get(name).copied();
        let score = if let Some(score) = reused {
            score
        } else {
            let score = calculate();
            if self.scores.len() < QUICK_OPEN_NAME_SCORE_CACHE_LIMIT {
                self.scores.insert(name, score);
            }
            score
        };
        if self.sampled < QUICK_OPEN_NAME_SCORE_SAMPLE {
            self.sampled += 1;
            self.reused += usize::from(reused.is_some());
            if self.sampled == QUICK_OPEN_NAME_SCORE_SAMPLE
                && self.reused < QUICK_OPEN_NAME_SCORE_MIN_REUSE
            {
                self.enabled = false;
                self.scores.clear();
            }
        }
        score
    }
}

impl QuickOpenRankingCache {
    fn rank(
        &mut self,
        entries: &std::sync::Arc<Vec<QuickOpenEntry>>,
        query: &str,
        mut cancelled: impl FnMut() -> bool,
    ) -> Option<Vec<(i64, PathBuf)>> {
        if cancelled() {
            return None;
        }
        let path_query = query.contains('/') || query.contains('\\');
        let normalized = query.to_lowercase().replace('\\', "/");
        let source = std::sync::Arc::downgrade(entries);
        let previous = self.completed.as_ref().filter(|previous| {
            previous.source.ptr_eq(&source) && previous.path_query == path_query
        });
        if let Some(previous) = previous.filter(|previous| previous.query == normalized) {
            return Some(previous.results.clone());
        }
        // Every match for an extended subsequence also matched its prefix.
        // A truncated eligibility list is never used to narrow later queries.
        let eligible = previous
            .filter(|previous| {
                normalized.len() > previous.query.len() && normalized.starts_with(&previous.query)
            })
            .and_then(|previous| previous.matches.as_deref());
        let count = eligible.map_or(entries.len(), <[usize]>::len);
        let mut matches = std::mem::take(&mut self.spare);
        matches.clear();
        let mut complete_matches = true;
        let mut matcher = crate::fuzzy_score::PreparedFuzzyScore::new(&normalized);
        let mut scored = QuickOpenCandidates::default();
        // Monorepos repeat names such as index.ts and package.json thousands
        // of times. Their name eligibility/score depends only on this query;
        // retain borrowed keys for this request, with a fixed memory bound.
        let mut name_scores = QuickOpenNameScores::default();
        #[cfg(test)]
        if self.force_direct_name_scores {
            name_scores.enabled = false;
        }
        for position in 0..count {
            if cancelled() {
                matches.clear();
                self.spare = matches;
                return None; // Keep the previous completed query untouched.
            }
            let index = eligible.map_or(position, |indices| indices[position]);
            let entry = &entries[index];
            let score =
                if path_query || count < QUICK_OPEN_NAME_SCORE_SAMPLE || !name_scores.enabled {
                    quick_open_score(&mut matcher, entry, path_query)
                } else {
                    let name_score = name_scores.score(&entry.name, || matcher.score(&entry.name));
                    quick_open_score_from_name(&mut matcher, entry, name_score)
                };
            if let Some(score) = score {
                if matches.len() < QUICK_OPEN_MATCH_CACHE_LIMIT {
                    matches.push(index);
                } else {
                    complete_matches = false;
                }
                scored.push_borrowed(score, &entry.path);
            }
        }
        let results = scored.into_sorted();
        if cancelled() {
            matches.clear();
            self.spare = matches;
            return None;
        }
        let matching_indices = complete_matches.then(|| std::mem::take(&mut matches));
        let previous = self.completed.replace(CompletedQuickOpenRanking {
            source,
            query: normalized,
            path_query,
            matches: matching_indices,
            results: results.clone(),
        });
        let mut spare = previous
            .and_then(|previous| previous.matches)
            .unwrap_or_default();
        if matches.capacity() > spare.capacity() {
            spare = matches;
        }
        spare.clear();
        self.spare = spare;
        Some(results)
    }
}

fn quick_open_score(
    matcher: &mut crate::fuzzy_score::PreparedFuzzyScore<'_>,
    entry: &QuickOpenEntry,
    path_query: bool,
) -> Option<i64> {
    if path_query {
        matcher.score(&entry.relative)
    } else {
        let name_score = matcher.score(&entry.name);
        quick_open_score_from_name(matcher, entry, name_score)
    }
}

fn quick_open_score_from_name(
    matcher: &mut crate::fuzzy_score::PreparedFuzzyScore<'_>,
    entry: &QuickOpenEntry,
    name_score: Option<i64>,
) -> Option<i64> {
    name_score.map(|score| {
        let relative_score = if entry.name == entry.relative {
            score // A root-level file has the identical name and path input.
        } else {
            matcher.score(&entry.relative).unwrap_or(0)
        };
        score + relative_score / 4
    })
}

#[cfg(test)]
fn rank_quick_open_files(
    entries: &[QuickOpenEntry],
    query: &str,
    cancel: &crate::background::CancellationToken,
) -> Vec<(i64, PathBuf)> {
    let path_query = query.contains('/') || query.contains('\\');
    let normalized = query.to_lowercase().replace('\\', "/");
    let mut matcher = crate::fuzzy_score::PreparedFuzzyScore::new(&normalized);
    let mut scored = QuickOpenCandidates::default();
    for entry in entries {
        if cancel.is_cancelled() {
            return Vec::new();
        }
        let score = quick_open_score(&mut matcher, entry, path_query);
        if let Some(score) = score {
            scored.push_borrowed(score, &entry.path);
        }
    }
    scored.into_sorted()
}

#[derive(Default)]
struct QuickOpenCandidates {
    // The worst retained match is at the root: low score, then high path.
    best: std::collections::BinaryHeap<(std::cmp::Reverse<i64>, PathBuf)>,
}

impl QuickOpenCandidates {
    fn push_borrowed(&mut self, score: i64, path: &std::path::Path) {
        if self.best.len() < QUICK_OPEN_RESULT_LIMIT
            || self.best.peek().is_some_and(|(worst_score, worst_path)| {
                std::cmp::Reverse(score) < *worst_score
                    || std::cmp::Reverse(score) == *worst_score && path < worst_path.as_path()
            })
        {
            self.push(score, path.to_path_buf());
        }
    }
    fn push(&mut self, score: i64, path: PathBuf) {
        let candidate = (std::cmp::Reverse(score), path);
        if self.best.len() < QUICK_OPEN_RESULT_LIMIT {
            self.best.push(candidate);
        } else if let Some(mut worst) = self.best.peek_mut()
            && candidate < *worst
        {
            *worst = candidate;
        }
    }

    fn into_sorted(self) -> Vec<(i64, PathBuf)> {
        self.best
            .into_sorted_vec()
            .into_iter()
            .map(|(std::cmp::Reverse(score), path)| (score, path))
            .collect()
    }
}

#[cfg(test)]
pub(crate) use crate::app_search_algo::preserve_case;
#[cfg(test)]
pub(crate) use crate::app_search_algo::regex_replace_with_groups;
#[cfg(test)]
pub(crate) use crate::app_search_algo::{is_whole_word_match, is_word_byte};

#[cfg(test)]
#[path = "app_replace_performance_tests.rs"]
mod replace_performance_tests;

// Apply nearby replacements with one rope mutation, retaining shared rope
// branches between distant matches. Coordinates are always from the snapshot;
// flushing from right to left keeps the remaining coordinates valid.
#[derive(Default)]
struct ReplacementBatch {
    edits: Vec<(usize, usize, String)>,
    inserted_bytes: usize,
}

impl ReplacementBatch {
    fn push(&mut self, rope: &mut ropey::Rope, start: usize, end: usize, text: String) {
        use crate::editor_text_ops::{DENSE_EDIT_MAX_BYTES, DENSE_EDIT_MAX_SPAN};
        if self.edits.len() == 4096
            || self
                .edits
                .first()
                .is_some_and(|edit| edit.1.saturating_sub(start) > DENSE_EDIT_MAX_SPAN)
            || self.inserted_bytes.saturating_add(text.len()) > DENSE_EDIT_MAX_BYTES
        {
            self.flush(rope);
        }
        self.inserted_bytes = self.inserted_bytes.saturating_add(text.len());
        self.edits.push((start, end, text));
    }

    fn flush(&mut self, rope: &mut ropey::Rope) {
        let edits = self
            .edits
            .iter()
            .rev()
            .map(|(start, end, text)| (*start, *end, text.as_str()));
        let dense = self
            .edits
            .first()
            .zip(self.edits.last())
            .is_some_and(|(last, first)| last.1 - first.0 <= self.edits.len().saturating_mul(256));
        if let Some((start, end, text)) = dense
            .then(|| crate::editor_text_ops::dense_edit_text(rope, edits))
            .flatten()
        {
            rope.remove(start..end);
            rope.insert(start, &text);
        } else {
            for (start, end, text) in &self.edits {
                rope.remove(*start..*end);
                rope.insert(*start, text);
            }
        }
        self.edits.clear();
        self.inserted_bytes = 0;
    }
}

struct ReplaceSnapshotOptions<'a> {
    line_numbers_are_one_based: bool,
    search_query: &'a str,
    replacement: &'a str,
    case_sensitive: bool,
    use_regex: bool,
    whole_word: bool,
    preserve: bool,
    selection: Option<(usize, usize, usize, usize)>,
}

#[cfg(test)]
fn replace_snapshot_matches(
    rope: ropey::Rope,
    matches: Vec<(usize, usize, usize)>,
    options: ReplaceSnapshotOptions<'_>,
) -> (ropey::Rope, usize) {
    let (prepared, count) = prepare_snapshot_matches(rope, matches, options);
    (prepared.rope().clone(), count)
}

fn prepare_snapshot_matches(
    rope: ropey::Rope,
    matches: Vec<(usize, usize, usize)>,
    options: ReplaceSnapshotOptions<'_>,
) -> (crate::formatters::Prepared, usize) {
    match SnapshotReplacer::new(options) {
        Ok(program) => program
            .prepare(
                rope.clone(),
                matches,
                &crate::background::CancellationToken::new(),
            )
            .unwrap_or_else(|_| {
                (
                    crate::formatters::Prepared::from_changes(
                        &rope,
                        rope.clone(),
                        Vec::new(),
                        false,
                    ),
                    0,
                )
            }),
        Err(_) => (
            crate::formatters::Prepared::from_changes(&rope, rope.clone(), Vec::new(), false),
            0,
        ),
    }
}

pub(crate) struct SnapshotReplacer<'a> {
    options: ReplaceSnapshotOptions<'a>,
    regex: crate::search_regex::SearchRegex,
    pattern: Option<crate::search_replace::Replacement>,
    workspace_pattern: Option<crate::search_replace::WorkspaceReplacement>,
}
impl<'a> SnapshotReplacer<'a> {
    pub(crate) fn workspace(
        options: &'a crate::workspace_replace::Options,
    ) -> Result<Self, String> {
        Self::new(ReplaceSnapshotOptions {
            line_numbers_are_one_based: true,
            search_query: &options.query,
            replacement: &options.replacement,
            case_sensitive: options.case_sensitive,
            use_regex: options.regex,
            whole_word: options.whole_word,
            preserve: options.preserve_case,
            selection: None,
        })
    }
    fn new(options: ReplaceSnapshotOptions<'a>) -> Result<Self, String> {
        use crate::search_regex::SearchRegex;
        let workspace = options.line_numbers_are_one_based;
        let regex = if workspace {
            SearchRegex::workspace(
                options.search_query,
                options.case_sensitive,
                options.whole_word,
                options.use_regex,
            )
        } else {
            SearchRegex::editor(
                options.search_query,
                options.case_sensitive,
                options.whole_word,
                options.use_regex,
            )
        }?;
        let workspace_pattern = (workspace && options.use_regex)
            .then(|| crate::search_replace::WorkspaceReplacement::new(options.replacement));
        let pattern = (!workspace).then(|| {
            crate::search_replace::Replacement::new(options.replacement, options.use_regex)
        });
        Ok(Self {
            options,
            regex,
            pattern,
            workspace_pattern,
        })
    }
    pub(crate) fn prepare(
        &self,
        mut rope: ropey::Rope,
        mut matches: Vec<(usize, usize, usize)>,
        cancel: &crate::background::CancellationToken,
    ) -> std::io::Result<(crate::formatters::Prepared, usize)> {
        use crate::search_regex::SearchText;
        let check = || {
            if cancel.is_cancelled() {
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        };
        check()?;
        let options = &self.options;
        let regex = &self.regex;
        let workspace = options.line_numbers_are_one_based;
        let stale = || {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Search matches changed before replacement",
            )
        };
        if !workspace && matches.len() >= crate::app_search_algo::MAX_SEARCH_MATCHES {
            matches = regex.scan(&rope, options.selection, usize::MAX, cancel).0;
        }
        let source = rope.clone();
        let bounds = options
            .selection
            .map(|(sl, sc, el, ec)| {
                let position = |row: usize, col: usize| {
                    (source.line_to_char(row.min(source.len_lines() - 1)) + col)
                        .min(source.len_chars())
                };
                (position(sl, sc), position(el, ec))
            })
            .unwrap_or((0, source.len_chars()));
        let document = regex
            .needs_document
            .then(|| SearchText::from_rope(source.slice(bounds.0..bounds.1), true));
        let mut line_buffer = None;
        let mut buffered_row = None;
        let validation_document = std::cell::OnceCell::new();
        let mut validation_line = None;
        let mut validation_row = None;
        let mut replaced = 0;
        let mut changes = Vec::new();
        let mut structure_changed = false;
        let mut batch = ReplacementBatch::default();
        let mut last_start = source.len_chars();
        let mut crlf = None;
        matches.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2)));
        matches.dedup();
        for (line_num, col_start, col_end) in matches {
            check()?;
            let row = if workspace {
                line_num.saturating_sub(1)
            } else {
                line_num
            };
            if workspace && line_num == 0
                || row >= source.len_lines()
                || col_start > source.line(row).len_chars()
            {
                if workspace {
                    return Err(stale());
                }
                continue;
            }
            let line_start = source.line_to_char(row);
            let start = line_start.saturating_add(col_start);
            let end = line_start.saturating_add(col_end);
            if start > end || end > source.len_chars() {
                if workspace {
                    return Err(stale());
                }
                continue;
            }
            // Disk workspace scans can return hits outside an explicitly
            // selected range. Those are intentionally excluded, not stale.
            if start < bounds.0 || end > bounds.1 {
                continue;
            }
            if end > last_start {
                if workspace {
                    return Err(stale());
                }
                continue;
            }
            let (input, offset) = if let Some(document) = &document {
                (document, source.char_to_byte(bounds.0))
            } else {
                if buffered_row != Some(row) {
                    let line = source.line(row);
                    let mut clean_len = line.len_chars();
                    while clean_len > 0 && matches!(line.char(clean_len - 1), '\r' | '\n') {
                        clean_len -= 1;
                    }
                    let clean_end = line_start + clean_len;
                    let low = line_start.max(bounds.0);
                    let high = clean_end.min(bounds.1);
                    if low > high {
                        if workspace {
                            return Err(stale());
                        }
                        continue;
                    }
                    let input = SearchText::from_rope(source.slice(low..high), true);
                    line_buffer = Some((input, source.char_to_byte(low)));
                    buffered_row = Some(row);
                }
                let (input, offset) = line_buffer.as_ref().unwrap();
                (input, *offset)
            };
            let source_start_byte = source.char_to_byte(start);
            let source_end_byte = source.char_to_byte(end);
            let start_byte = input.normalized_offset(source_start_byte - offset);
            let end_byte = input.normalized_offset(source_end_byte - offset);
            if start_byte > input.text.len() || end_byte > input.text.len() {
                if workspace {
                    return Err(stale());
                }
                continue;
            }
            let mut actual_replacement = if workspace {
                // Search results can outlive a disk change or buffer edit. The
                // engine that produced workspace matches must still match exactly
                // this range before template/capture fallback rules may run.
                let (validation, validation_start, validation_end) = if options.selection.is_some()
                    && regex.needs_document
                {
                    let full = validation_document.get_or_init(|| {
                        SearchText::from_rope(source.slice(..), true)
                    });
                    (
                        full,
                        full.normalized_offset(source_start_byte),
                        full.normalized_offset(source_end_byte),
                    )
                } else if options.selection.is_some()
                    && (bounds.0 > line_start
                        || bounds.1 < line_start + source.line(row).len_chars())
                {
                    // A zero-width match at the selection edge can depend on
                    // lookaround outside that selection. Validate against the
                    // same full line the workspace scanner saw, while keeping
                    // the established template preview context below intact.
                    if validation_row != Some(row) {
                        let line = source.line(row);
                        let mut end = line.len_chars();
                        while end > 0 && matches!(line.char(end - 1), '\r' | '\n') {
                            end -= 1;
                        }
                        validation_line = Some(SearchText::from_rope(line.slice(..end), true));
                        validation_row = Some(row);
                    }
                    let full = validation_line.as_ref().unwrap();
                    let offset = source.char_to_byte(line_start);
                    (
                        full,
                        full.normalized_offset(source_start_byte - offset),
                        full.normalized_offset(source_end_byte - offset),
                    )
                } else {
                    (input, start_byte, end_byte)
                };
                if regex.find_at(&validation.text, validation_start)
                    != Some(validation_start..validation_end)
                {
                    // A miss may scan the rest of a long line. Stop this file
                    // immediately instead of repeating that scan for every
                    // stale result (quadratic work on a dense changed line).
                    return Err(stale());
                }
                if !options.use_regex {
                    if options.preserve {
                        let matched = SearchText::from_rope(source.slice(start..end), true);
                        crate::search_replace::preserve_case(&matched.text, options.replacement)
                    } else {
                        options.replacement.to_owned()
                    }
                } else {
                    let pattern = self.workspace_pattern.as_ref().unwrap();
                    if !pattern.needs_captures() {
                        // A static template needs no capture iterator/state or
                        // copied match text unless preserve-case was requested.
                        if options.preserve {
                            let matched = SearchText::from_rope(source.slice(start..end), true);
                            crate::search_replace::preserve_case(&matched.text, pattern.fallback())
                        } else {
                            pattern.fallback().to_owned()
                        }
                    } else {
                        // Match.replaceString tries the match text first, then its preview
                        // context, and finally falls back to the parsed static template.
                        let matched = SearchText::from_rope(source.slice(start..end), true);
                        if let Some(captures) = regex.captures_at(&matched.text, 0) {
                            pattern.expand(&matched.text, &captures, options.preserve)
                        } else if let Some(captures) = regex
                            .captures_at(&input.text, start_byte)
                            .filter(|captures| captures.range.start == start_byte)
                        {
                            pattern.expand(&input.text, &captures, options.preserve)
                        } else {
                            pattern.fallback().to_owned()
                        }
                    }
                }
            } else {
                let Some(captures) = regex.captures_at(&input.text, start_byte) else {
                    if workspace {
                        return Err(stale());
                    }
                    continue;
                };
                if captures.range != (start_byte..end_byte) {
                    if workspace {
                        return Err(stale());
                    }
                    continue;
                }
                self.pattern
                    .as_ref()
                    .unwrap()
                    .expand(&input.text, &captures, options.preserve)
            };
            if actual_replacement.contains('\r') {
                actual_replacement = actual_replacement.replace("\r\n", "\n").replace('\r', "\n");
            }
            if actual_replacement.contains('\n')
                && *crlf.get_or_insert_with(|| source.chunks().any(|chunk| chunk.contains("\r\n")))
            {
                actual_replacement = actual_replacement.replace('\n', "\r\n");
            }
            if source.slice(start..end) != actual_replacement.as_str() {
                structure_changed |= source.slice(start..end).len_lines() > 1
                    || actual_replacement.contains(['\r', '\n']);
                changes.push(crate::rope_changes::EditOffset {
                    start,
                    end,
                    new_end: actual_replacement.chars().count(),
                });
                batch.push(&mut rope, start, end, actual_replacement);
            }
            last_start = start;
            replaced += 1;
        }
        batch.flush(&mut rope);
        changes.reverse();
        let (mut old_end, mut new_end) = (0, 0);
        for change in &mut changes {
            new_end += change.start - old_end + change.new_end;
            old_end = change.end;
            change.new_end = new_end;
        }
        check()?;
        Ok((
            crate::formatters::Prepared::from_changes(&source, rope, changes, structure_changed),
            replaced,
        ))
    }
}
pub(crate) fn apply_open_replace_result(
    tabs: &mut [crate::app::Tab],
    result: &crate::background::OpenFileReplaceResult,
) -> bool {
    let mut applied = false;
    for tab in tabs {
        if tab.kind != crate::app::TabKind::File {
            continue;
        }
        let editor = &mut tab.editor;
        editor.refresh_document();
        if editor.file_path.as_ref() != Some(&result.path)
            || editor.save_identity != result.source_document_id
            || editor.content_version != result.source_content_version
        {
            continue;
        }
        // Applying publishes once to the shared document. Later views refresh
        // to its new version and cannot record another copy of this edit.
        applied |= result.prepared.apply(editor).is_ok();
    }
    applied
}

impl App {
    fn file_search_results_ready(&mut self) -> bool {
        // Shared documents can change between input events without changing
        // this view's cached Rope. Never navigate or edit using stale offsets.
        let _ = self.editor_mut();
        let current = self.editor_content_target();
        let matches_current = self
            .file_search_matches_target
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(previous, current)| {
                previous.same_editor(current)
                    && previous.content_version == current.content_version
                    && previous.len_bytes == current.len_bytes
            });
        if !matches_current {
            self.update_search();
            return false;
        }
        !self.bg_file_search_running && !self.file_search_refresh_pending
    }

    pub(crate) fn adjust_search_selection_after_replace(
        &mut self,
        before: &ropey::Rope,
        after: &ropey::Rope,
    ) {
        let Some((sl, sc, el, ec)) = self
            .search_selection_range
            .filter(|_| self.search_in_selection)
        else {
            return;
        };
        let old_end =
            (before.line_to_char(el.min(before.len_lines() - 1)) + ec).min(before.len_chars());
        let end = if after.len_chars() >= before.len_chars() {
            old_end.saturating_add(after.len_chars() - before.len_chars())
        } else {
            old_end.saturating_sub(before.len_chars() - after.len_chars())
        }
        .min(after.len_chars());
        let start = (before.line_to_char(sl.min(before.len_lines() - 1)) + sc).min(end);
        let start_row = after.char_to_line(start);
        let end_row = after.char_to_line(end);
        self.search_selection_range = Some((
            start_row,
            start - after.line_to_char(start_row),
            end_row,
            end - after.line_to_char(end_row),
        ));
    }

    pub(crate) fn search_next(&mut self) {
        self.navigate_search(true);
    }

    pub(crate) fn search_prev(&mut self) {
        self.navigate_search(false);
    }

    fn navigate_search(&mut self, forward: bool) {
        if self.bg_file_search_running
            || self.file_search_refresh_pending
            || self.search_matches.is_empty()
        {
            return;
        }
        if !self.file_search_results_ready() {
            return;
        }
        let Some(editor) = self.editor() else {
            return;
        };
        let rope = editor.rope.clone();
        let (sl, sc, el, ec) = editor.selection_range().unwrap_or((
            editor.cursor_line,
            editor.cursor_col,
            editor.cursor_line,
            editor.cursor_col,
        ));
        let mut position = if forward { (el, ec) } else { (sl, sc) };
        let matches = if self.search_truncated {
            let Ok(regex) = crate::search_regex::SearchRegex::editor(
                &self.search_query,
                self.search_case_sensitive,
                self.search_whole_word,
                self.search_regex,
            ) else {
                return;
            };
            std::borrow::Cow::Owned(
                regex
                    .scan(
                        &rope,
                        self.search_in_selection
                            .then_some(self.search_selection_range)
                            .flatten(),
                        usize::MAX,
                        &crate::background::CancellationToken::new(),
                    )
                    .0,
            )
        } else {
            std::borrow::Cow::Borrowed(self.search_matches.as_slice())
        };
        if matches.is_empty() {
            return;
        }
        let end_position = |&(row, _, end): &(usize, usize, usize)| {
            let offset = (rope.line_to_char(row) + end).min(rope.len_chars());
            let last = rope.char_to_line(offset);
            (last, offset - rope.line_to_char(last))
        };
        let choose = |position: (usize, usize)| {
            if forward {
                let index = matches.partition_point(|&(row, start, _)| (row, start) < position);
                if index == matches.len() { 0 } else { index }
            } else {
                let index = matches.partition_point(|found| end_position(found) <= position);
                if index == 0 {
                    matches.len() - 1
                } else {
                    index - 1
                }
            }
        };
        let mut index = choose(position);
        if matches[index].1 == matches[index].2 && (matches[index].0, matches[index].1) == position
        {
            // VS Code steps to the next line for empty anchor matches.
            let line_stops = self.search_regex && self.search_query.contains(['^', '$']);
            position = if forward {
                if line_stops || position.1 >= editor.line_len(position.0) {
                    ((position.0 + 1) % rope.len_lines(), 0)
                } else {
                    (position.0, position.1 + 1)
                }
            } else if line_stops || position.1 == 0 {
                let row = if position.0 == 0 {
                    rope.len_lines() - 1
                } else {
                    position.0 - 1
                };
                (row, editor.line_len(row))
            } else {
                (position.0, position.1 - 1)
            };
            index = choose(position);
        }
        let found = matches[index];
        drop(matches);
        if forward && (found.0, found.1) < (el, ec) {
            self.notify("Wrapped past end of file");
        }
        if !forward && end_position(&found) > (sl, sc) {
            self.notify("Wrapped past beginning of file");
        }
        self.search_current = match self.search_matches.binary_search(&found) {
            Ok(index) => index,
            Err(_) => {
                // Keep the selected match available past the decoration limit.
                self.search_matches
                    .truncate(crate::app_search_algo::MAX_SEARCH_MATCHES - 1);
                let index = self
                    .search_matches
                    .partition_point(|&candidate| candidate < found);
                self.search_matches.insert(index, found);
                index
            }
        };
        self.jump_to_current_match();
    }

    /// Push current search query to history (called when search is committed).
    pub(crate) fn push_search_history(&mut self) {
        let query = self.search_query.clone();
        self.search_history.record(&query);
    }

    /// Navigate search history backwards (older).
    pub(crate) fn search_history_prev(&mut self) {
        let current = self.search_query.clone();
        if let Some(query) = self.search_history.previous(&current) {
            self.search_query = query;
            self.search_cursor = self.search_query.len();
            self.search_selection_anchor = None;
            self.update_search();
        }
    }

    /// Navigate search history forwards (newer).
    pub(crate) fn search_history_next(&mut self) {
        if let Some(query) = self.search_history.next() {
            self.search_query = query;
            self.search_cursor = self.search_query.len();
            self.search_selection_anchor = None;
            self.update_search();
        }
    }

    pub(crate) fn jump_to_current_match(&mut self) {
        if !self.file_search_results_ready() {
            return;
        }
        if let Some(&(line, col, end)) = self.search_matches.get(self.search_current) {
            self.push_nav_history();
            let diff = self.row_diff_active();
            if let Some(editor) = self.editor_mut() {
                editor.push_cursor_position();
                editor.clear_selection();
                let offset = (editor.rope.line_to_char(line) + end).min(editor.rope.len_chars());
                let end_line = editor.rope.char_to_line(offset);
                editor.set_cursor(end_line, offset - editor.rope.line_to_char(end_line));
                editor.selection_anchor = Some((line, col));
                if !diff {
                    editor.center_cursor();
                }
            }
            if diff {
                let cache = self.diff_render_cache.clone();
                let mut scroll = std::mem::take(&mut self.diff_inline_scroll);
                if let Some(editor) = self.editor_mut() {
                    scroll.layout(cache, editor);
                    scroll.center_cursor(editor);
                }
                self.diff_inline_scroll = scroll;
            }
            self.finish_navigation();
        }
    }

    pub(crate) fn replace_current(&mut self) {
        if self.bg_file_search_running
            || self.file_search_refresh_pending
            || self.bg_file_replace_running
        {
            return;
        }
        if !self.file_search_results_ready() {
            return;
        }
        let Some(&found @ (line, col_start, col_end)) =
            self.search_matches.get(self.search_current)
        else {
            return;
        };
        let Some(editor) = self
            .editor()
            .filter(|editor| !editor.read_only && !editor.bg_loading)
        else {
            return;
        };
        let source = editor.rope.clone();
        if line >= source.len_lines() {
            self.update_search();
            return;
        }
        let start = source.line_to_char(line) + col_start;
        let end = source.line_to_char(line) + col_end;
        if start > end || end > source.len_chars() {
            self.update_search();
            return;
        }
        let old_length = source.len_chars();
        let (prepared, replaced) = prepare_snapshot_matches(
            source.clone(),
            vec![found],
            ReplaceSnapshotOptions {
                line_numbers_are_one_based: false,
                search_query: &self.search_query,
                replacement: &self.replace_query,
                case_sensitive: self.search_case_sensitive,
                use_regex: self.search_regex,
                whole_word: self.search_whole_word,
                preserve: self.search_preserve_case,
                selection: self
                    .search_in_selection
                    .then_some(self.search_selection_range)
                    .flatten(),
            },
        );
        if replaced == 0 {
            return;
        }
        let rope = prepared.rope();
        let inserted = rope.len_chars() - (old_length - (end - start));
        let mut next = start + inserted;
        if start == end && inserted == 0 {
            next = if next < rope.len_chars() { next + 1 } else { 0 };
        }
        let mut changed = false;
        if let Some(editor) = self.editor_mut() {
            let Ok(edits) = prepared.apply(editor) else {
                return;
            };
            changed = edits > 0;
            let next_line = editor.rope.char_to_line(next);
            editor.clear_selection();
            editor.set_cursor(next_line, next - editor.rope.line_to_char(next_line));
        }
        self.adjust_search_selection_after_replace(&source, rope);
        self.last_edit_time = std::time::Instant::now();
        // Resume after the inserted text, including for zero-length matches.
        if changed {
            self.search_current = usize::MAX;
            self.update_search();
        } else {
            // Identical replacements still advance Find, but must not create
            // an undo entry, dirty the file, or erase an existing redo branch.
            let row = rope.char_to_line(next);
            let position = (row, next - rope.line_to_char(row));
            let index = self
                .search_matches
                .partition_point(|&(row, col, _)| (row, col) < position);
            self.search_current = if index < self.search_matches.len() {
                index
            } else {
                0
            };
        }
        self.jump_to_current_match();
    }

    pub(crate) fn apply_file_replacement(
        &mut self,
        prepared: &crate::formatters::Prepared,
        replacements: usize,
    ) {
        let Some(editor) = self.editor_mut() else {
            return;
        };
        let before = editor.rope.clone();
        if let Err(error) = prepared.apply(editor) {
            self.update_search();
            self.notify(format!("Replace all cancelled: {error}"));
            return;
        }
        self.adjust_search_selection_after_replace(&before, prepared.rope());
        self.last_edit_time = std::time::Instant::now();
        self.notify(format!("Replaced {replacements} occurrences"));
        self.update_search();
    }

    pub(crate) fn replace_all(&mut self) {
        if !self.file_search_results_ready() {
            return;
        }
        if self.bg_file_search_running
            || self.file_search_refresh_pending
            || self
                .editor()
                .is_none_or(|editor| editor.read_only || editor.bg_loading)
        {
            return;
        }
        if self.search_matches.is_empty() {
            self.notify("No matches to replace");
            return;
        }
        if self.bg_file_replace_running {
            self.notify("Replace all is already running");
            return;
        }
        #[cfg(not(test))]
        let Some(target) = self.editor_content_target() else {
            return;
        };
        let Some(rope) = self.editor().map(|editor| editor.rope.clone()) else {
            return;
        };
        let matches = std::mem::take(&mut self.search_matches);
        // The worker now owns these results, including when its output turns
        // out to be identical and the document version remains unchanged.
        self.file_search_key = None;
        let replacement = self.replace_query.clone();
        let preserve = self.search_preserve_case;
        let use_regex = self.search_regex;
        let whole_word = self.search_whole_word;
        let search_query = self.search_query.clone();
        let case_sensitive = self.search_case_sensitive;
        let selection = self
            .search_in_selection
            .then_some(self.search_selection_range)
            .flatten();
        self.file_replace_generation = self.file_replace_generation.wrapping_add(1);
        #[cfg(not(test))]
        let generation = self.file_replace_generation;
        #[cfg(not(test))]
        let source_document_id = self.editor().unwrap().save_identity;

        #[cfg(test)]
        {
            let (prepared, replacements) = prepare_snapshot_matches(
                rope,
                matches,
                ReplaceSnapshotOptions {
                    line_numbers_are_one_based: false,
                    search_query: &search_query,
                    replacement: &replacement,
                    case_sensitive,
                    use_regex,
                    whole_word,
                    preserve,
                    selection,
                },
            );
            self.apply_file_replacement(&prepared, replacements);
        }

        #[cfg(not(test))]
        {
            self.bg_file_replace_running = true;
            self.status_message = "Replacing matches...".to_string();
            let sender = self.background.sender.clone();
            self.background.submit_interactive(move |_| {
                let (prepared, replacements) = prepare_snapshot_matches(
                    rope,
                    matches,
                    ReplaceSnapshotOptions {
                        line_numbers_are_one_based: false,
                        search_query: &search_query,
                        replacement: &replacement,
                        case_sensitive,
                        use_regex,
                        whole_word,
                        preserve,
                        selection,
                    },
                );
                let _ = sender.send(crate::background::BackgroundResult::FileReplaceDone {
                    generation,
                    target,
                    source_document_id,
                    prepared,
                    replacements,
                });
            });
        }
    }

    /// Replace all occurrences across all files found by global search.
    pub(crate) fn replace_all_in_files(&mut self) {
        if self.global_search_results.is_empty() {
            self.notify("No matches to replace");
            return;
        }
        if self.bg_global_search_running {
            self.notify("Wait for the search to finish before replacing all matches");
            return;
        }
        if self.bg_global_replace_running {
            self.notify("Workspace replacement is already running");
            return;
        }

        use crate::workspace_replace::{Job, Options, Snapshot, Target};
        let mut by_file: rustc_hash::FxHashMap<
            PathBuf,
            (Vec<(usize, usize, usize)>, Option<Snapshot>),
        > = rustc_hash::FxHashMap::default();
        for found in &self.global_search_results {
            by_file.entry(found.file_path.clone()).or_default().0.push((
                found.line_num,
                found.col_start,
                found.col_end,
            ));
        }
        // One pass over every desk's views, not a tab scan per matched file.
        // Hidden desk panes live in groups and share the same documents/history.
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            if tab.kind != crate::app::TabKind::File {
                continue;
            }
            let Some((_, snapshot)) = tab
                .editor
                .file_path
                .as_ref()
                .and_then(|path| by_file.get_mut(path))
            else {
                continue;
            };
            if snapshot.is_some() {
                continue;
            }
            tab.editor.refresh_document();
            *snapshot = Some(Snapshot {
                identity: tab.editor.save_identity,
                version: tab.editor.content_version,
                rope: tab.editor.rope.clone(),
                writable: !tab.editor.read_only && tab.editor.save_content_error().is_none(),
            });
        }
        let targets = by_file
            .into_iter()
            .map(|(path, (matches, snapshot))| {
                let pending_save = self.pending_file_saves.contains_key(&path);
                Target {
                    path,
                    matches,
                    snapshot,
                    pending_save,
                }
            })
            .collect::<Vec<_>>();
        self.global_replace_generation = self.global_replace_generation.wrapping_add(1);
        self.bg_global_replace_running = true;
        self.workspace_replace.open = Default::default();
        self.status_message = format!("Replacing matches in {} files...", targets.len());
        let job = Job {
            workspace_generation: self.workspace_generation,
            generation: self.global_replace_generation,
            targets,
            options: Options {
                query: self.global_search_query.clone(),
                replacement: self.global_search_replace_query.clone(),
                case_sensitive: self.global_search_case_sensitive,
                regex: self.global_search_regex,
                whole_word: self.global_search_whole_word,
                preserve_case: self.global_search_preserve_case,
            },
            remote: self.remote_workspace.clone(),
        };
        let sender = self.background.sender.clone();
        self.workspace_replace.cancel = Some(
            self.background
                .spawn_cancellable("workspace-replace", move |cancel| job.run(cancel, sender)),
        );
    }

    pub(crate) fn cancel_workspace_replace(&mut self) {
        self.workspace_replace.cancel();
        self.global_replace_generation = self.global_replace_generation.wrapping_add(1);
        self.bg_global_replace_running = false;
    }

    pub(crate) fn apply_workspace_replace_prepared(
        &mut self,
        workspace: u64,
        generation: u64,
        open_files: Vec<crate::background::OpenFileReplaceResult>,
        failures: usize,
    ) -> bool {
        if workspace != self.workspace_generation
            || generation != self.global_replace_generation
            || !self.bg_global_replace_running
            || self
                .workspace_replace
                .cancel
                .as_ref()
                .is_some_and(|cancel| cancel.is_cancelled())
        {
            return false;
        }
        let mut changed = false;
        self.workspace_replace.open.failures += failures;
        for result in open_files {
            let mut applied = apply_open_replace_result(&mut self.tabs, &result);
            for group in &mut self.groups {
                applied |= apply_open_replace_result(&mut group.tabs, &result);
            }
            if applied {
                self.workspace_replace.open.replacements += result.replacements;
                self.workspace_replace.open.files_changed += 1;
                changed = true;
            } else {
                self.workspace_replace.open.failures += 1;
            }
        }
        if changed {
            self.last_edit_time = std::time::Instant::now();
            if self.search_active {
                self.update_search();
            }
        }
        changed
    }

    pub(crate) fn finish_workspace_replace(
        &mut self,
        workspace: u64,
        generation: u64,
        mut outcome: crate::workspace_replace::Outcome,
        candidates: rustc_hash::FxHashSet<PathBuf>,
        error: Option<String>,
    ) -> bool {
        if workspace != self.workspace_generation
            || generation != self.global_replace_generation
            || !self.bg_global_replace_running
        {
            return false;
        }
        self.bg_global_replace_running = false;
        outcome.cancelled |= self
            .workspace_replace
            .cancel
            .take()
            .is_some_and(|cancel| cancel.is_cancelled());
        let disk_changed = outcome.written_files > 0;
        outcome.add(std::mem::take(&mut self.workspace_replace.open));
        let count = outcome.replacements;
        let files = outcome.files_changed;
        if let Some(error) = error {
            self.notify_warn(format!("Workspace replacement did not finish: {error}"));
        } else if outcome.cancelled {
            self.notify(format!(
                "Stopped replacement after {count} occurrences in {files} files"
            ));
        } else {
            self.notify(format!("Replaced {count} occurrences in {files} files"));
        }
        if outcome.failures > 0 {
            self.notify_warn(format!(
                "Skipped {} files that failed or changed during replacement",
                outcome.failures
            ));
        }
        // A formerly closed file may have been opened/edited while disk work
        // ran. Check only such live buffers. The worker built the candidate set,
        // so the UI never scans/hashes the full workspace result list here.
        let paths = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == crate::app::TabKind::File)
            .filter_map(|tab| {
                tab.editor
                    .file_path
                    .as_ref()
                    .filter(|path| candidates.contains(*path))
                    .cloned()
            })
            .collect::<Vec<_>>();
        self.queue_file_disk_checks(paths);
        if disk_changed {
            self.refresh_explorer_async();
            self.refresh_source_control_async();
        }
        self.global_search_results.clear();
        self.update_global_search();
        true
    }

    pub(crate) fn update_quick_open(&mut self) {
        self.quick_open_results.clear();
        // Cancel any in-flight quick-open
        if let Some(token) = self.quick_open_cancel_token.take() {
            token.cancel();
        }
        self.quick_open_generation = self.quick_open_generation.wrapping_add(1);
        self.bg_quick_open_running = false;

        let settings = self.settings_for_scope(crate::settings::SettingsScope::Workspace);
        let excludes: crate::settings::ExcludePatterns = settings.files_exclude.into_iter().chain(settings.search_exclude).collect();
        if self.quick_open_query.is_empty() && self.remote_workspace.is_none()
            && !excludes.values().any(serde_json::Value::is_object) {
            let rules = crate::search_paths::ExcludeRules::new(&excludes, &Default::default());
            self.quick_open_results = self.recent_files.iter().filter(|path| !rules.excludes_with(
                &crate::search_paths::Candidate::new(path), std::path::Path::new(""), |_| false)).cloned().collect();
            self.quick_open_selected = 0;
            return;
        }

        let generation = self.quick_open_generation;
        self.bg_quick_open_running = true;
        let root = self.explorer.root.clone();
        let query = self.quick_open_query.clone();
        let index = self.quick_open_index.clone();
        let index_generation = self.workspace_file_index_generation;
        let watched = self.workspace_watcher_active;
        let sender = self.background.sender.clone();
        self.quick_open_selected = 0;
        let remote = self.remote_workspace.clone();
        let recent = self.quick_open_query.is_empty().then(|| self.recent_files.clone());
        let token = index.submit_latest(&mut self.background, move |index, cancel| {
            if let Some(paths) = recent {
                let files = if let Some(remote) = &remote {
                    let indices = remote.search_history_scope(&paths, &excludes, &cancel).unwrap_or_default();
                    indices.into_iter().map(|i| (0, paths[i].clone())).collect()
                } else {
                    let rules = crate::search_paths::ExcludeRules::new(&excludes, &Default::default());
                    paths.into_iter().filter(|path| !rules.excludes(&crate::search_paths::Candidate::new(path), std::path::Path::new("")))
                        .map(|path| (0, path)).collect()
                };
                if !cancel.is_cancelled() { let _ = sender.send(crate::background::BackgroundResult::QuickOpenFiles { files, generation }); }
                return;
            }
            let Some(entries) =
                index.files_with_remote(&root, index_generation, watched, &cancel, remote.as_ref(), &excludes)
            else {
                return;
            };
            let Some(files) = index.rank_files(&entries, &query, &cancel) else {
                return;
            };
            if cancel.is_cancelled() {
                return;
            }
            let _ = sender
                .send(crate::background::BackgroundResult::QuickOpenFiles { files, generation }); // Error OK: receiver dropped means no listener
        });
        self.quick_open_cancel_token = Some(token);
    }

    pub(crate) fn build_symbol_list(&mut self) {
        self.request_source_symbols(crate::source_symbols::Destination::Picker(
            self.symbol_list_workspace,
        ));
    }

    pub(crate) fn update_outline(&mut self) {
        self.request_source_symbols(crate::source_symbols::Destination::Outline);
    }

    pub(crate) fn filter_symbol_items(&mut self) {
        let query = self.symbol_list_query.to_lowercase();
        self.symbol_list_filtered.clear();
        if query.is_empty() {
            self.symbol_list_filtered
                .extend(0..self.symbol_list_items.len());
        } else {
            self.symbol_list_filtered.extend(
                self.symbol_list_items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| fuzzy_match(&item.name, &query))
                    .map(|(i, _)| i),
            );
        }
        self.symbol_list_selected = 0;
    }

    pub(crate) fn update_symbol_filter(&mut self) {
        self.filter_symbol_items();
        // Fire workspace/symbol LSP request on query change
        if self.symbol_list_workspace && !self.lsp_manager.clients.is_empty() {
            self.lsp_manager
                .request_workspace_symbol(&self.symbol_list_query);
        }
    }

    pub(crate) fn open_goto_line(&mut self) {
        self.goto_line_query.clear();
        self.goto_line_cursor = 0;
        self.goto_line_selection_anchor = None;
        let saved = self.editor().map(|ed| {
            let selection = crate::app_navigation::NavigationSelection::capture(ed).unwrap_or(
                crate::app_navigation::NavigationSelection {
                    cursor: (ed.cursor_line, ed.cursor_col),
                    anchor: ed.selection_anchor,
                },
            );
            (selection, ed.scroll_offset)
        });
        self.goto_line_saved_selection = saved.map(|(selection, _)| selection);
        self.goto_line_saved_scroll = saved.map(|(_, scroll)| scroll);
        self.goto_line_active = true;
    }

    pub(crate) fn restore_goto_line_origin(&mut self) {
        if let (Some(selection), Some(scroll)) =
            (self.goto_line_saved_selection, self.goto_line_saved_scroll)
        {
            if let Some(editor) = self.editor_mut() {
                selection.restore(editor);
                editor.scroll_offset = scroll;
            }
        }
    }

    pub(crate) fn preview_goto_line(&mut self) {
        let mut parts = self.goto_line_query.split(':');
        if let Ok(line_num) = parts.next().unwrap_or_default().trim().parse::<usize>() {
            let col = parts
                .next()
                .and_then(|part| part.trim().parse::<usize>().ok())
                .unwrap_or(1)
                .saturating_sub(1);
            if let Some(editor) = self.editor_mut() {
                let target = line_num.saturating_sub(1);
                editor.set_cursor(target, col);
                editor.center_cursor();
            }
        }
    }

    pub(crate) fn cancel_goto_line(&mut self) {
        self.restore_goto_line_origin();
        self.goto_line_active = false;
        self.goto_line_query.clear();
        self.goto_line_cursor = 0;
        self.goto_line_selection_anchor = None;
        self.goto_line_saved_selection = None;
        self.goto_line_saved_scroll = None;
    }

    pub(crate) fn goto_line(&mut self) {
        // Support "line" or "line:col" format
        let mut parts = self.goto_line_query.split(':');
        let parsed_line = parts.next().unwrap_or_default().trim().parse::<usize>();
        let col = parts
            .next()
            .and_then(|part| part.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .saturating_sub(1);
        let query_was_nonempty = !self.goto_line_query.is_empty();
        if let Ok(line_num) = parsed_line {
            // Restore original position first so nav history captures the real origin
            self.restore_goto_line_origin();
            self.push_nav_history();
            if let Some(editor) = self.editor_mut() {
                editor.push_cursor_position();
                editor.clear_selection();
                let target = line_num.saturating_sub(1);
                let max_line = editor.line_count();
                editor.set_cursor(target, col);
                editor.center_cursor();
                if !editor.bg_loading && line_num > max_line {
                    self.notify(format!(
                        "Line {} exceeds file length ({} lines), jumped to last line",
                        line_num, max_line
                    ));
                }
            }
        } else {
            // Invalid input — restore original position and notify
            self.restore_goto_line_origin();
            if query_was_nonempty {
                self.notify("Invalid line number");
            }
        }
        self.goto_line_active = false;
        self.goto_line_query.clear();
        self.goto_line_cursor = 0;
        self.goto_line_selection_anchor = None;
        self.goto_line_saved_selection = None;
        self.goto_line_saved_scroll = None;
        self.finish_navigation();
    }

    /// Accept the currently selected LSP completion item
    pub(crate) fn apply_lsp_completion(&mut self) {
        if !self.completion_snapshot_current() {
            self.lsp_completion_active = false;
            self.dispatch_lsp_suggestions();
            return;
        }
        if !self.lsp_completion_active || self.lsp_manager.completion_items.is_empty() {
            return;
        }
        let idx = self
            .lsp_completion_selected
            .min(self.lsp_manager.completion_items.len().saturating_sub(1));
        let item = self.lsp_manager.completion_items[idx].clone();
        if let Some(editor) = self.editor_mut() {
            if let Err(error) = crate::app_lsp_edits::complete(editor, &item) {
                self.notify_error(error);
            }
        }
        self.lsp_completion_active = false;
        self.lsp_manager.completion_items.clear();
        self.dismiss_vtcomplete();
        // After accepting LSP completion, dismiss stale ghost text and trigger re-request
    }

    pub(crate) fn apply_lsp_code_action(&mut self) {
        if !self.lsp_code_action_active || self.lsp_manager.code_actions.is_empty() {
            return;
        }
        let idx = self
            .lsp_code_action_selected
            .min(self.lsp_manager.code_actions.len().saturating_sub(1));
        let action = self.lsp_manager.code_actions[idx].clone();
        self.lsp_code_action_active = false;
        self.lsp_manager.code_actions.clear();

        let path = self.editor().and_then(|editor| editor.file_path.clone());
        let command = path.zip(action.command);
        if let Some(edit) = action.edit {
            let versions = std::mem::take(&mut self.lsp_manager.code_action_request_versions);
            self.queue_lsp_workspace_edit_snapshot(
                edit,
                format!("Applied: {}", action.title),
                versions,
                command,
            );
            self.poll_lsp_workspace_edits();
        } else if let Some((path, command)) = command {
            if self.lsp_manager.execute_command(&path, &command) {
                self.notify(format!("Running: {}", action.title));
            } else {
                self.notify_error("Language server command is unavailable");
            }
        } else {
            self.notify(format!("Code action: {} (no edit)", action.title));
        }
    }
}

#[cfg(test)]
#[path = "quick_open_ranking_performance_tests.rs"]
mod ranking_performance_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn lsp_snippet_prefix_replacement_matches_original_across_rope_chunks() {
        let prefix = "界é separator ".repeat(2_000);
        for (text, col) in [
            (String::new(), 0),
            ("café_界 suffix".to_owned(), 6),
            ("foo.bar_suffix".to_owned(), 7),
            ("foo. suffix".to_owned(), 4),
            ("é界\r\nnext".to_owned(), 2),
            (
                format!("{prefix}café_界 suffix"),
                prefix.chars().count() + 6,
            ),
        ] {
            for body in ["${1:name}($2)$0", "${1:界}\n$0"] {
                let mut expected = crate::editor::Editor::test_with_text(&text);
                expected.cursor_col = col;
                let before: String = expected.rope.line(0).chars().take(col).collect();
                let word_start_byte = before
                    .rfind(|c: char| !c.is_alphanumeric() && c != '_')
                    .map_or(0, |i| i + before[i..].chars().next().unwrap().len_utf8());
                let start = before[..word_start_byte].chars().count();
                expected.save_undo();
                if col > start {
                    expected.rope.remove(start..col);
                }
                expected.insert_snippet_text(start, body);

                let mut app = search_app(&text);
                app.editor_mut().unwrap().cursor_col = col;
                app.lsp_completion_active = true;
                app.lsp_manager.completion_items = vec![crate::lsp::LspCompletionItem {
                    label: "snippet".into(),
                    detail: None,
                    insert_text: body.into(),
                    kind: None,
                    text_edit_range: None,
                    additional_text_edits: Vec::new(),
                    is_snippet: true,
                    original_item: None,
                    documentation: None,
                }];
                app.apply_lsp_completion();
                let actual = app.editor().unwrap();
                assert_eq!(actual.rope, expected.rope);
                assert_eq!(
                    (
                        actual.cursor_line,
                        actual.cursor_col,
                        actual.selection_anchor
                    ),
                    (
                        expected.cursor_line,
                        expected.cursor_col,
                        expected.selection_anchor
                    )
                );
                assert_eq!(actual.snippet_active, expected.snippet_active);
                let stops = |editor: &crate::editor::Editor| {
                    editor
                        .snippet_tab_stops
                        .iter()
                        .map(|stop| (stop.index, stop.line, stop.col, stop.placeholder_len))
                        .collect::<Vec<_>>()
                };
                assert_eq!(stops(actual), stops(&expected));
                assert!(!app.lsp_completion_active && app.lsp_manager.completion_items.is_empty());
                app.editor_mut().unwrap().undo();
                assert_eq!(app.editor().unwrap().rope.to_string(), text);
            }
        }
    }

    #[test]
    fn shared_source_symbol_parser_preserves_visibility_and_unicode_names() {
        for (line, expected) in [
            (
                "    pub(crate) async fn café() {}",
                Some(("café", "function")),
            ),
            ("pub struct Data<T> {", Some(("Data<T", "struct"))),
            (
                "pub(crate) const LIMIT: usize = 5;",
                Some(("LIMIT", "constant")),
            ),
            ("async fn load_data() {}", Some(("load_data", "function"))),
            ("class Widget:", Some(("Widget", "class"))),
            ("let fn_name = 10;", None),
        ] {
            assert_eq!(super::source_symbol(line), expected, "{line}");
        }
    }
    use std::path::PathBuf;

    use crate::app::{App, Tab};
    use crate::editor::Editor;

    #[test]
    fn preserve_case_all_upper() {
        assert_eq!(super::preserve_case("HELLO", "world"), "WORLD");
    }

    #[test]
    fn preserve_case_all_lower() {
        assert_eq!(super::preserve_case("hello", "WORLD"), "world");
    }

    #[test]
    fn preserve_case_title_case() {
        assert_eq!(super::preserve_case("Hello", "world"), "World");
    }

    #[test]
    fn preserve_case_empty_input() {
        assert_eq!(super::preserve_case("", "world"), "world");
        assert_eq!(super::preserve_case("hello", ""), "");
    }

    #[test]
    fn preserve_case_no_alpha() {
        assert_eq!(super::preserve_case("123", "world"), "WORLD");
    }

    #[test]
    fn preserve_case_unicode_upper() {
        assert_eq!(super::preserve_case("CAFÉ", "hello"), "HELLO");
    }

    #[test]
    fn preserve_case_unicode_lower() {
        assert_eq!(super::preserve_case("café", "HELLO"), "hello");
    }

    #[test]
    fn preserve_case_unicode_title() {
        assert_eq!(super::preserve_case("Café", "hello"), "Hello");
    }

    #[test]
    fn preserve_case_mixed_with_numbers() {
        // "Hello123" — first upper, rest lower+digits → title case
        assert_eq!(super::preserve_case("Hello123", "world"), "World");
    }

    #[test]
    fn preserve_case_numbers_between_alpha() {
        // "ABC123DEF" — all alpha chars are upper → all upper
        assert_eq!(super::preserve_case("ABC123DEF", "test"), "TEST");
    }

    #[test]
    fn preserve_case_mixed_no_pattern() {
        // "hElLo" — not all upper, not all lower, not title → unchanged
        assert_eq!(super::preserve_case("hElLo", "world"), "world");
    }

    #[test]
    fn preserve_case_single_char_upper() {
        assert_eq!(super::preserve_case("A", "hello"), "HELLO");
    }

    #[test]
    fn preserve_case_single_char_lower() {
        assert_eq!(super::preserve_case("a", "HELLO"), "hello");
    }

    #[test]
    fn is_whole_word_match_at_boundaries() {
        assert!(super::is_whole_word_match("hello world", 0, 5)); // "hello"
        assert!(super::is_whole_word_match("hello world", 6, 11)); // "world"
    }

    #[test]
    fn is_whole_word_match_in_middle() {
        assert!(!super::is_whole_word_match("helloworld", 0, 5)); // not a word boundary
    }

    #[test]
    fn is_whole_word_match_with_underscores() {
        assert!(!super::is_whole_word_match("hello_world", 0, 5)); // underscore is word char
    }

    #[test]
    fn is_whole_word_match_entire_string() {
        assert!(super::is_whole_word_match("hello", 0, 5));
    }

    // --- regex_replace_with_groups tests ---

    #[test]
    fn regex_replace_basic() {
        let result = super::regex_replace_with_groups("foo", "foo", "bar", true);
        assert_eq!(result, "bar");
    }

    #[test]
    fn regex_replace_capture_group() {
        let result = super::regex_replace_with_groups(r"(\w+)-(\w+)", "hello-world", "$2-$1", true);
        assert_eq!(result, "world-hello");
    }

    #[test]
    fn regex_replace_case_insensitive() {
        let result = super::regex_replace_with_groups("foo", "FOO", "bar", false);
        assert_eq!(result, "bar");
    }

    #[test]
    fn regex_replace_invalid_pattern() {
        let result = super::regex_replace_with_groups("[invalid", "text", "replacement", true);
        assert_eq!(result, "text"); // Invalid regex should return original text
    }

    // --- is_word_byte tests ---

    #[test]
    fn is_word_byte_alpha() {
        assert!(super::is_word_byte(b'a'));
        assert!(super::is_word_byte(b'Z'));
    }

    #[test]
    fn is_word_byte_digit() {
        assert!(super::is_word_byte(b'0'));
        assert!(super::is_word_byte(b'9'));
    }

    #[test]
    fn is_word_byte_underscore() {
        assert!(super::is_word_byte(b'_'));
    }

    #[test]
    fn is_word_byte_non_word() {
        assert!(!super::is_word_byte(b' '));
        assert!(!super::is_word_byte(b'-'));
        assert!(!super::is_word_byte(b'.'));
    }

    #[test]
    fn regex_replace_invalid_pattern_returns_original() {
        use super::regex_replace_with_groups;
        // Invalid regex should return original matched text, not replacement
        let result = regex_replace_with_groups("[invalid", "hello", "world", true);
        assert_eq!(result, "hello", "invalid regex should return original text");
    }

    #[test]
    fn regex_replace_valid_pattern_with_groups() {
        use super::regex_replace_with_groups;
        let result = regex_replace_with_groups(r"(\w+) (\w+)", "hello world", "$2 $1", true);
        assert_eq!(result, "world hello");
    }

    // ===== Search-and-replace integration tests =====

    #[test]
    fn quick_open_bounded_candidates_match_full_sort() {
        for count in [0, 5, 20, 21, 1_000] {
            let candidates: Vec<_> = (0..count)
                .map(|i| {
                    (
                        (i * 7919 % 31) as i64 - 15,
                        PathBuf::from(format!("file-{i:04}")),
                    )
                })
                .collect();
            let mut expected = candidates.clone();
            expected.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            expected.truncate(super::QUICK_OPEN_RESULT_LIMIT);
            for reverse in [false, true] {
                let mut input = candidates.clone();
                if reverse {
                    input.reverse();
                }
                let mut bounded = super::QuickOpenCandidates::default();
                for (score, path) in input {
                    bounded.push_borrowed(score, &path);
                    assert!(bounded.best.len() <= super::QUICK_OPEN_RESULT_LIMIT);
                }
                assert_eq!(bounded.into_sorted(), expected);
            }
        }
    }

    fn legacy_rank_quick_open(
        entries: &[super::QuickOpenEntry],
        root: &std::path::Path,
        query: &str,
    ) -> Vec<(i64, PathBuf)> {
        use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
        // A fresh engine is the score oracle: the pinned engine's retained
        // matrix bonuses can otherwise make ranks depend on earlier queries.
        let fuzzy_score =
            |choice: &str, query: &str| SkimMatcherV2::default().fuzzy_match(choice, query);
        let query = query.to_lowercase();
        let normalized = query.replace('\\', "/");
        let path_query = query.contains('/') || query.contains('\\');
        let mut scored = super::QuickOpenCandidates::default();
        for entry in entries {
            let path = &entry.path;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_lowercase();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_lowercase()
                .replace('\\', "/");
            let score = if path_query {
                fuzzy_score(&relative, &normalized)
            } else {
                fuzzy_score(&name, &query)
                    .map(|score| score + fuzzy_score(&relative, &query).unwrap_or(0) / 4)
            };
            if let Some(score) = score {
                scored.push(score, path.clone());
            }
        }
        scored.into_sorted()
    }

    #[test]
    fn quick_open_index_reuses_names_and_refreshes_workspace_generations() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        std::fs::create_dir_all(root.join("src/界")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("src/界/Café.rs"), "").unwrap();
        std::fs::write(root.join("src/library.rs"), "").unwrap();
        std::fs::write(root.join(".hidden"), "").unwrap();
        std::fs::write(root.join("node_modules/skipped.rs"), "").unwrap();
        let index = super::QuickOpenIndex::default();
        let cancel = crate::background::CancellationToken::new();
        let files = index.files(root, 1, true, &cancel).unwrap();
        assert_eq!(files.len(), 3);
        assert!(std::sync::Arc::ptr_eq(
            &files,
            &index.files(root, 1, true, &cancel).unwrap()
        ));
        for query in ["ca", "CAFÉ", "src/cf", "src\\界", "rs", "missing"] {
            assert_eq!(
                super::rank_quick_open_files(&files, query, &cancel),
                legacy_rank_quick_open(&files, root, query),
                "{query}"
            );
        }
        std::fs::write(root.join("new.rs"), "").unwrap();
        let refreshed = index.files(root, 2, true, &cancel).unwrap();
        assert_eq!(refreshed.len(), 4);
        assert!(!std::sync::Arc::ptr_eq(&files, &refreshed));
        index.files.lock().unwrap().as_mut().unwrap().built_at -= std::time::Duration::from_secs(3);
        std::fs::write(root.join("external.rs"), "").unwrap();
        assert_eq!(index.files(root, 2, false, &cancel).unwrap().len(), 5);
        index.files.lock().unwrap().as_mut().unwrap().built_at -=
            std::time::Duration::from_secs(31);
        std::fs::write(root.join("remote.rs"), "").unwrap();
        assert_eq!(index.files(root, 2, true, &cancel).unwrap().len(), 6);
        cancel.cancel();
        assert!(index.files(root, 2, true, &cancel).is_none());
    }

    #[test]
    fn clearing_quick_open_rejects_an_already_queued_query_result() {
        let mut app = App::test_new();
        let old_generation = app.quick_open_generation;
        app.recent_files = vec![PathBuf::from("recent.rs")];
        app.quick_open_query.clear();
        app.update_quick_open();
        app.background
            .sender
            .send(crate::background::BackgroundResult::QuickOpenFiles {
                generation: old_generation,
                files: vec![(10, PathBuf::from("stale.rs"))],
            })
            .unwrap();
        app.poll_background_results();
        assert_eq!(app.quick_open_results, [PathBuf::from("recent.rs")]);
        assert!(!app.bg_quick_open_running);
    }

    #[test]
    fn workspace_switch_retires_old_quick_open_results_and_requeries_visible_picker() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let old_path = first.path().join("alpha.rs");
        let new_path = second.path().join("beta.rs");
        std::fs::write(&old_path, "old source").unwrap();
        std::fs::write(&new_path, "new source").unwrap();
        let mut app = App::new(first.path().to_path_buf());
        let old_index = app.quick_open_index.clone();
        let old_cancel = crate::background::CancellationToken::new();
        app.quick_open_cancel_token = Some(old_cancel.clone());
        app.quick_open_active = true;
        app.quick_open_query = "beta".into();
        let generation = app.quick_open_generation;
        app.change_workspace(second.path());
        assert!(old_cancel.is_cancelled());
        assert!(app.quick_open_active);
        assert_eq!(app.quick_open_query, "beta");
        assert!(!std::sync::Arc::ptr_eq(
            &old_index.files,
            &app.quick_open_index.files
        ));
        app.background
            .sender
            .send(crate::background::BackgroundResult::QuickOpenFiles {
                generation,
                files: vec![(99, old_path.clone())],
            })
            .unwrap();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            !app.bg_quick_open_running
        });
        assert_eq!(app.quick_open_results, vec![new_path]);
        assert!(!app.quick_open_results.contains(&old_path));
    }

    #[test]
    #[ignore = "manual comparative release benchmark"]
    fn benchmark_quick_open_cached_candidates() {
        let root = std::path::Path::new("workspace");
        let entries: Vec<_> = (0..50_000)
            .map(|index| {
                let relative = format!("src/components/component-{index:05}.rs");
                super::QuickOpenEntry {
                    path: root.join(&relative),
                    name: format!("component-{index:05}.rs"),
                    relative,
                }
            })
            .collect();
        let cancel = crate::background::CancellationToken::new();
        let expected = legacy_rank_quick_open(&entries, root, "cmp");
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for cached in [sample % 2 == 0, sample % 2 != 0] {
                let started = std::time::Instant::now();
                let results = if cached {
                    super::rank_quick_open_files(std::hint::black_box(&entries), "cmp", &cancel)
                } else {
                    legacy_rank_quick_open(std::hint::black_box(&entries), root, "cmp")
                };
                let elapsed = started.elapsed();
                assert_eq!(std::hint::black_box(results), expected);
                if cached {
                    after.push(elapsed);
                } else {
                    before.push(elapsed);
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "Quick Open 50,000 candidates normalization/ranking (filesystem excluded): before={:?} after={:?}",
            before[3], after[3]
        );
    }

    #[test]
    #[ignore = "manual performance measurement; run with --release --ignored --nocapture"]
    fn benchmark_quick_open_ranking() {
        use std::hint::black_box;
        use std::time::Instant;

        let candidates: Vec<_> = (0..100_000)
            .map(|i| {
                (
                    (i * 7919 % 100_003) as i64,
                    PathBuf::from(format!("src/file-{i:06}.rs")),
                )
            })
            .collect();
        let mut expected = candidates.clone();
        expected.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.0));
        expected.truncate(super::QUICK_OPEN_RESULT_LIMIT);
        for bounded in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..7 {
                let mut input = black_box(candidates.clone());
                let started = Instant::now();
                let result = if bounded {
                    let mut best = super::QuickOpenCandidates::default();
                    for (score, path) in input {
                        best.push(score, path);
                    }
                    best.into_sorted()
                } else {
                    input.sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.0));
                    input.truncate(super::QUICK_OPEN_RESULT_LIMIT);
                    input
                };
                samples.push(started.elapsed());
                assert_eq!(result, expected);
                black_box(result);
            }
            samples.sort();
            eprintln!(
                "quick_open_ranking bounded={bounded} candidates=100000 median={:?}",
                samples[3]
            );
        }
    }

    #[test]
    fn replace_snapshot_reuses_regex_with_unicode_captures_and_newlines() {
        for one_based in [false, true] {
            let line = usize::from(one_based);
            let (rope, count) = super::replace_snapshot_matches(
                ropey::Rope::from_str("é Foo-12 foo-34\n"),
                vec![(line, 2, 8), (line, 9, 15)],
                super::ReplaceSnapshotOptions {
                    line_numbers_are_one_based: one_based,
                    search_query: r"(?<word>foo)-(\d+)",
                    replacement: "$1:$2\n✓",
                    case_sensitive: false,
                    use_regex: true,
                    whole_word: false,
                    preserve: false,
                    selection: None,
                },
            );
            assert_eq!(count, 2);
            assert_eq!(rope.to_string(), "é Foo:12\n✓ foo:34\n✓\n");
        }
    }

    #[test]
    fn replace_snapshot_invalid_regex_keeps_original_text() {
        let (rope, count) = super::replace_snapshot_matches(
            ropey::Rope::from_str("foo bar"),
            vec![(0, 0, 3), (0, 4, 7)],
            super::ReplaceSnapshotOptions {
                line_numbers_are_one_based: false,
                search_query: "[invalid",
                replacement: "replacement",
                case_sensitive: true,
                use_regex: true,
                whole_word: false,
                preserve: true,
                selection: None,
            },
        );
        assert_eq!(count, 0);
        assert_eq!(rope.to_string(), "foo bar");
    }

    #[test]
    #[ignore = "manual performance measurement; run with --release --ignored --nocapture"]
    fn benchmark_replace_snapshot() {
        use std::hint::black_box;
        use std::time::Instant;

        let rope = ropey::Rope::from_str(&"hello-123\n".repeat(2_000));
        let matches: Vec<_> = (0..2_000).map(|line| (line, 0, 9)).collect();
        for use_regex in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..5 {
                let snapshot = rope.clone();
                let matches = matches.clone();
                let started = Instant::now();
                let (result, count) = super::replace_snapshot_matches(
                    black_box(snapshot),
                    black_box(matches),
                    super::ReplaceSnapshotOptions {
                        line_numbers_are_one_based: false,
                        search_query: r"(\w+)-(\d+)",
                        replacement: if use_regex { "$2:$1" } else { "replacement" },
                        case_sensitive: true,
                        use_regex,
                        whole_word: false,
                        preserve: false,
                        selection: None,
                    },
                );
                samples.push(started.elapsed());
                assert_eq!(count, 2_000);
                assert_eq!(
                    result.to_string(),
                    if use_regex {
                        "123:hello\n"
                    } else {
                        "replacement\n"
                    }
                    .repeat(2_000)
                );
                black_box(result);
            }
            samples.sort();
            eprintln!(
                "replace_snapshot regex={use_regex} matches=2000 median={:?}",
                samples[2]
            );
        }
    }

    fn search_app(text: &str) -> App {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(text);
        editor.cursor_line = 0;
        editor.cursor_col = 0;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app
    }

    fn rope_text(app: &App) -> String {
        app.tabs[app.active_tab].editor.rope.to_string()
    }

    #[test]
    fn search_reuses_identical_completed_and_pending_requests() {
        let mut app = search_app("needle other needle");
        app.search_query = "needle".into();
        app.update_search();
        let generation = app.file_search_generation;
        let matches_ptr = app.search_matches.as_ptr();
        app.editor_mut().unwrap().cursor_col = 10;
        app.update_search();
        assert_eq!(app.file_search_generation, generation);
        assert_eq!(app.search_matches.as_ptr(), matches_ptr);

        let token = crate::background::CancellationToken::new();
        app.bg_file_search_running = true;
        app.file_search_cancel_token = Some(token.clone());
        app.update_search();
        assert_eq!(app.file_search_generation, generation);
        assert!(!token.is_cancelled());

        app.search_whole_word = true;
        app.update_search();
        assert!(token.is_cancelled());
        assert!(app.file_search_generation > generation);
    }

    #[test]
    fn search_selection_does_not_spend_the_match_limit_outside_selection() {
        let text = format!("{}needle\n", "needle\n".repeat(20_000));
        let mut app = search_app(&text);
        app.search_query = "needle".into();
        app.search_in_selection = true;
        app.search_selection_range = Some((20_000, 0, 20_000, 6));
        for regex in [false, true] {
            app.search_regex = regex;
            app.update_search();
            assert_eq!(app.search_matches, vec![(20_000, 0, 6)]);
            assert!(!app.search_truncated);
        }
    }

    #[test]
    fn search_basic_finds_all_matches() {
        let mut app = search_app("foo bar foo baz foo");
        app.search_query = "foo".to_string();
        app.update_search();
        assert_eq!(app.search_matches.len(), 3);
        assert_eq!(app.search_matches[0], (0, 0, 3));
        assert_eq!(app.search_matches[1], (0, 8, 11));
        assert_eq!(app.search_matches[2], (0, 16, 19));
    }

    #[test]
    fn search_case_insensitive() {
        let mut app = search_app("Hello HELLO hello");
        app.search_query = "hello".to_string();
        app.search_case_sensitive = false;
        app.update_search();
        assert_eq!(app.search_matches.len(), 3);
    }

    #[test]
    fn search_case_sensitive() {
        let mut app = search_app("Hello HELLO hello");
        app.search_query = "hello".to_string();
        app.search_case_sensitive = true;
        app.update_search();
        assert_eq!(app.search_matches.len(), 1);
        assert_eq!(app.search_matches[0], (0, 12, 17));
    }

    #[test]
    fn search_whole_word() {
        let mut app = search_app("foo foobar barfoo foo");
        app.search_query = "foo".to_string();
        app.search_whole_word = true;
        app.update_search();
        assert_eq!(app.search_matches.len(), 2);
        assert_eq!(app.search_matches[0], (0, 0, 3));
        assert_eq!(app.search_matches[1], (0, 18, 21));
    }

    #[test]
    fn search_regex_basic() {
        let mut app = search_app("abc 123 def 456");
        app.search_query = r"\d+".to_string();
        app.search_regex = true;
        app.update_search();
        assert_eq!(app.search_matches.len(), 2);
        assert_eq!(app.search_matches[0], (0, 4, 7));
        assert_eq!(app.search_matches[1], (0, 12, 15));
    }

    #[test]
    fn search_multiline() {
        let mut app = search_app("line1 foo\nline2 foo\nline3 bar");
        app.search_query = "foo".to_string();
        app.update_search();
        assert_eq!(app.search_matches.len(), 2);
        assert_eq!(app.search_matches[0].0, 0); // line 0
        assert_eq!(app.search_matches[1].0, 1); // line 1
    }

    #[test]
    fn search_empty_query_no_matches() {
        let mut app = search_app("some text");
        app.search_query = String::new();
        app.update_search();
        assert!(app.search_matches.is_empty());
    }

    #[test]
    fn search_no_match() {
        let mut app = search_app("hello world");
        app.search_query = "xyz".to_string();
        app.update_search();
        assert!(app.search_matches.is_empty());
    }

    #[test]
    fn search_next_wraps_around() {
        let mut app = search_app("a a a");
        app.search_query = "a".to_string();
        app.update_search();
        assert_eq!(app.search_matches.len(), 3);
        assert_eq!(app.search_current, 0);
        app.jump_to_current_match();
        app.search_next();
        assert_eq!(app.search_current, 1);
        app.search_next();
        assert_eq!(app.search_current, 2);
        app.search_next(); // wraps
        assert_eq!(app.search_current, 0);
    }

    #[test]
    fn atomic_search_refresh_preserves_the_current_match_position() {
        let mut app = search_app("foo foo foo");
        app.search_query = "foo".to_string();
        app.update_search();
        app.search_current = 1;
        let target = app.editor_content_target().expect("editor target");

        app.apply_file_search_matches(target, vec![(0, 0, 3), (0, 5, 8), (0, 9, 12)], false, None);

        assert_eq!(app.search_current, 1);
    }

    #[test]
    fn search_prev_wraps_around() {
        let mut app = search_app("a a a");
        app.search_query = "a".to_string();
        app.update_search();
        assert_eq!(app.search_current, 0);
        app.jump_to_current_match();
        app.search_prev(); // wraps to last
        assert_eq!(app.search_current, 2);
        app.search_prev();
        assert_eq!(app.search_current, 1);
    }

    #[test]
    fn replace_current_basic() {
        let mut app = search_app("foo bar foo");
        app.search_query = "foo".to_string();
        app.replace_query = "baz".to_string();
        app.update_search();
        assert_eq!(app.search_matches.len(), 2);
        app.replace_current();
        assert_eq!(rope_text(&app), "baz bar foo");
        assert_eq!(app.search_matches.len(), 1); // re-searched after replace
    }

    #[test]
    fn replace_current_twice() {
        let mut app = search_app("foo foo foo");
        app.search_query = "foo".to_string();
        app.replace_query = "x".to_string();
        app.update_search();
        app.replace_current();
        assert_eq!(rope_text(&app), "x foo foo");
        app.replace_current();
        assert_eq!(rope_text(&app), "x x foo");
    }

    #[test]
    fn replace_all_basic() {
        let mut app = search_app("foo bar foo baz foo");
        app.search_query = "foo".to_string();
        app.replace_query = "qux".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "qux bar qux baz qux");
    }

    #[test]
    fn replace_all_with_longer_replacement() {
        let mut app = search_app("a b a");
        app.search_query = "a".to_string();
        app.replace_query = "xyz".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "xyz b xyz");
    }

    #[test]
    fn replace_all_with_shorter_replacement() {
        let mut app = search_app("hello world hello");
        app.search_query = "hello".to_string();
        app.replace_query = "hi".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "hi world hi");
    }

    #[test]
    fn replace_all_with_empty_replacement() {
        let mut app = search_app("foo bar foo");
        app.search_query = "foo".to_string();
        app.replace_query = String::new();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), " bar ");
    }

    #[test]
    fn replace_all_multiline() {
        let mut app = search_app("foo\nbar\nfoo\nbaz");
        app.search_query = "foo".to_string();
        app.replace_query = "qux".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "qux\nbar\nqux\nbaz");
    }

    #[test]
    fn replace_current_no_matches_noop() {
        let mut app = search_app("hello world");
        app.search_query = "xyz".to_string();
        app.replace_query = "abc".to_string();
        app.update_search();
        app.replace_current(); // should not panic
        assert_eq!(rope_text(&app), "hello world");
    }

    #[test]
    fn replace_all_no_matches_noop() {
        let mut app = search_app("hello world");
        app.search_query = "xyz".to_string();
        app.replace_query = "abc".to_string();
        app.update_search();
        app.replace_all(); // should not panic
        assert_eq!(rope_text(&app), "hello world");
    }

    #[test]
    fn replace_all_regex_with_capture_groups() {
        let mut app = search_app("John Smith\nJane Doe");
        app.search_query = r"(\w+) (\w+)".to_string();
        app.search_regex = true;
        app.replace_query = "$2, $1".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "Smith, John\nDoe, Jane");
    }

    #[test]
    fn replace_all_case_insensitive() {
        let mut app = search_app("Foo FOO foo");
        app.search_query = "foo".to_string();
        app.search_case_sensitive = false;
        app.replace_query = "bar".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "bar bar bar");
    }

    #[test]
    fn replace_all_preserve_case() {
        let mut app = search_app("Hello HELLO hello");
        app.search_query = "hello".to_string();
        app.search_case_sensitive = false;
        app.search_preserve_case = true;
        app.replace_query = "world".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "World WORLD world");
    }

    #[test]
    fn replace_all_whole_word() {
        let mut app = search_app("foo foobar barfoo foo");
        app.search_query = "foo".to_string();
        app.search_whole_word = true;
        app.replace_query = "baz".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "baz foobar barfoo baz");
    }

    #[test]
    fn replace_all_clears_matches_when_done() {
        let mut app = search_app("foo foo");
        app.search_query = "foo".to_string();
        app.replace_query = "bar".to_string();
        app.update_search();
        assert_eq!(app.search_matches.len(), 2);
        app.replace_all();
        // After replace_all, update_search is called; "foo" no longer exists
        assert!(app.search_matches.is_empty());
    }

    #[test]
    fn search_history_push_and_navigate() {
        let mut app = search_app("text");
        app.search_query = "first".to_string();
        app.push_search_history();
        app.search_query = "second".to_string();
        app.push_search_history();
        app.search_query = "third".to_string();
        app.push_search_history();

        app.search_query = "current".to_string();
        app.search_history_prev(); // should go to "third"
        assert_eq!(app.search_query, "third");
        app.search_history_prev(); // should go to "second"
        assert_eq!(app.search_query, "second");
        app.search_history_next(); // back to "third"
        assert_eq!(app.search_query, "third");
        app.search_history_next(); // past most recent, restore "current"
        assert_eq!(app.search_query, "current");
    }

    #[test]
    fn search_history_dedup() {
        let mut app = search_app("text");
        app.search_query = "foo".to_string();
        app.push_search_history();
        app.search_query = "bar".to_string();
        app.push_search_history();
        app.search_query = "foo".to_string();
        app.push_search_history();
        // "foo" should appear only once (most recent position)
        assert_eq!(app.search_history.saved(""), ["bar", "foo"]);
    }

    #[test]
    fn search_regex_invalid_pattern_shows_error() {
        let mut app = search_app("text");
        app.search_query = "[invalid".to_string();
        app.search_regex = true;
        app.update_search();
        assert!(app.search_matches.is_empty());
        assert!(app.status_message.contains("Invalid regex"));
    }

    #[test]
    fn replace_current_undo_restores_text() {
        let mut app = search_app("foo bar");
        app.search_query = "foo".to_string();
        app.replace_query = "baz".to_string();
        app.update_search();
        app.replace_current();
        assert_eq!(rope_text(&app), "baz bar");
        // Undo should restore
        if let Some(ed) = app.editor_mut() {
            ed.undo();
        }
        assert_eq!(rope_text(&app), "foo bar");
    }

    #[test]
    fn replace_all_undo_restores_all() {
        let mut app = search_app("foo foo foo");
        app.search_query = "foo".to_string();
        app.replace_query = "bar".to_string();
        app.update_search();
        app.replace_all();
        assert_eq!(rope_text(&app), "bar bar bar");
        // Single undo should restore all replacements (single undo checkpoint)
        if let Some(ed) = app.editor_mut() {
            ed.undo();
        }
        assert_eq!(rope_text(&app), "foo foo foo");
    }

    #[test]
    fn search_regex_whole_word() {
        let mut app = search_app("cat catch category cat");
        app.search_query = "cat".to_string();
        app.search_regex = true;
        app.search_whole_word = true;
        app.update_search();
        assert_eq!(app.search_matches.len(), 2);
        assert_eq!(app.search_matches[0], (0, 0, 3));
        assert_eq!(app.search_matches[1], (0, 19, 22));
    }

    #[test]
    fn replace_replacement_containing_search_term() {
        let mut app = search_app("a b a");
        app.search_query = "a".to_string();
        app.replace_query = "aa".to_string();
        app.update_search();
        app.replace_all();
        // replace_all uses a snapshot of matches, so doubling should work
        assert_eq!(rope_text(&app), "aa b aa");
    }

    // ===== Goto-line dialog tests =====

    #[test]
    fn goto_line_basic() {
        let mut app = search_app("line1\nline2\nline3\nline4\nline5\n");
        app.open_goto_line();
        app.goto_line_query = "3".to_string();
        app.goto_line();
        let ed = app.editor().unwrap();
        assert_eq!(ed.cursor_line, 2); // 0-indexed
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn goto_line_with_column() {
        let mut app = search_app("line1\nline2\nline3\nline4\n");
        app.open_goto_line();
        app.goto_line_query = "2:4".to_string();
        app.goto_line();
        let ed = app.editor().unwrap();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 3); // 1-indexed col -> 0-indexed
    }

    #[test]
    fn goto_line_beyond_file_end() {
        let mut app = search_app("line1\nline2\n");
        app.open_goto_line();
        app.goto_line_query = "999".to_string();
        app.goto_line();
        let ed = app.editor().unwrap();
        // set_cursor clamps to last line
        assert!(ed.cursor_line <= 2);
    }

    #[test]
    fn goto_line_zero_goes_to_first_line() {
        let mut app = search_app("line1\nline2\n");
        app.open_goto_line();
        app.goto_line_query = "0".to_string();
        app.goto_line();
        let ed = app.editor().unwrap();
        assert_eq!(ed.cursor_line, 0); // 0.saturating_sub(1) = 0
    }

    #[test]
    fn goto_line_cancel_restores_cursor() {
        let mut app = search_app("line1\nline2\nline3\n");
        // Position cursor at line 1, col 3
        if let Some(ed) = app.editor_mut() {
            ed.cursor_line = 1;
            ed.cursor_col = 3;
        }
        app.open_goto_line();
        // Preview moves cursor to line 3
        app.goto_line_query = "3".to_string();
        app.preview_goto_line();
        let ed = app.editor().unwrap();
        assert_eq!(ed.cursor_line, 2);
        // Cancel should restore
        app.cancel_goto_line();
        let ed = app.editor().unwrap();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn goto_line_empty_query_restores() {
        let mut app = search_app("line1\nline2\n");
        if let Some(ed) = app.editor_mut() {
            ed.cursor_line = 0;
            ed.cursor_col = 2;
        }
        app.open_goto_line();
        app.goto_line_query = "".to_string();
        app.goto_line();
        let ed = app.editor().unwrap();
        // Invalid input: restores original
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn goto_line_whitespace_query_restores() {
        let mut app = search_app("line1\nline2\n");
        if let Some(ed) = app.editor_mut() {
            ed.cursor_line = 0;
            ed.cursor_col = 1;
        }
        app.open_goto_line();
        app.goto_line_query = "   ".to_string();
        app.goto_line();
        let ed = app.editor().unwrap();
        // Whitespace-only can't parse as usize -> restores
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn goto_line_preview_moves_cursor() {
        let mut app = search_app("line1\nline2\nline3\n");
        app.open_goto_line();
        app.goto_line_query = "2".to_string();
        app.preview_goto_line();
        let ed = app.editor().unwrap();
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn goto_line_dialog_state_cleared_after_confirm() {
        let mut app = search_app("line1\nline2\n");
        app.open_goto_line();
        assert!(app.goto_line_active);
        app.goto_line_query = "1".to_string();
        app.goto_line();
        assert!(!app.goto_line_active);
        assert!(app.goto_line_query.is_empty());
        assert!(app.goto_line_saved_selection.is_none());
    }

    #[test]
    fn goto_line_dialog_state_cleared_after_cancel() {
        let mut app = search_app("line1\nline2\n");
        app.open_goto_line();
        assert!(app.goto_line_active);
        app.goto_line_query = "2".to_string();
        app.cancel_goto_line();
        assert!(!app.goto_line_active);
        assert!(app.goto_line_query.is_empty());
        assert!(app.goto_line_saved_selection.is_none());
    }
}
