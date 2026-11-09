use crate::app::{App, GlobalSearchResult};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use crate::search_regex::SearchRegex;
use crate::search_walk::{MatchedFiles, OpenFileAliases, has_parent_traversal};

type OpenSearchBuffers = rustc_hash::FxHashMap<PathBuf, ropey::Rope>;

const MODEL_SEARCH_QUIET_PERIOD: Duration = Duration::from_millis(75);

struct ModelUpdate {
    serial: u64,
    rope: ropey::Rope,
}

#[derive(Default)]
pub(crate) struct ModelUpdates {
    pending_since: Option<Instant>,
    pending: rustc_hash::FxHashMap<PathBuf, ModelUpdate>,
    serial: u64,
    work: crate::background::latest::LatestWork,
}

impl ModelUpdates {
    fn clear(&mut self) {
        self.work.cancel();
        self.pending.clear();
        self.pending_since = None;
    }
}

#[cfg(test)]
#[path = "regex_search_tests.rs"]
mod regex_tests;

#[cfg(test)]
#[path = "app_remote_search_tests.rs"]
mod remote_tests;

#[cfg(test)]
#[path = "workspace_search_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
#[path = "app_global_search_debounce_tests.rs"]
mod debounce_tests;

#[cfg(test)]
#[path = "workspace_search_dismiss_tests.rs"]
mod dismiss_tests;

#[cfg(test)]
#[path = "workspace_search_edit_tests.rs"]
mod edit_tests;

#[cfg(test)]
#[path = "workspace_search_navigation_tests.rs"]
mod navigation_tests;

fn search_failed(sender: &crate::background::BackgroundResultSender, generation: u64, error: String) {
    let _ = sender.send(crate::background::BackgroundResult::GlobalSearchFailed {
        generation, error,
    });
}

pub(crate) const EXCLUDE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    "dist",
    "build",
    ".mypy_cache",
    ".pytest_cache",
];

#[derive(Clone, Copy)]
pub(crate) struct GlobalSearchSpec<'a> {
    pub query: &'a str,
    pub regex: Option<&'a SearchRegex>,
    pub include: &'a str,
    pub exclude: &'a str,
    pub max_results: usize,
    pub case_sensitive: bool,
    pub open_buffers: Option<&'a OpenSearchBuffers>,
}

#[cfg(test)]
fn next_global_match(
    text: &str,
    start: usize,
    spec: GlobalSearchSpec<'_>,
    finder: &memchr::memmem::Finder<'_>,
) -> Option<(usize, usize)> {
    if start > text.len() {
        return None;
    }
    if let Some(regex) = spec.regex {
        regex.find_at(text, start).map(|found| (found.start, found.end))
    } else {
        finder.find(&text.as_bytes()[start..]).map(|pos| {
            let begin = start + pos;
            (begin, begin + spec.query.len())
        })
    }
}

impl App {
    pub(crate) fn can_replace_global_search_result(&self, index: usize) -> bool {
        self.global_search_replace_visible
            && !self.bg_global_search_running
            && !self.bg_global_replace_running
            && self.global_search_error.is_none()
            && self.global_search_results.get(index).is_some_and(|result|
                !self.global_search_model_updates.pending.contains_key(&result.file_path))
    }

    pub(crate) fn submit_global_search(&mut self, run: impl FnOnce(crate::background::CancellationToken) + Send + 'static) -> crate::background::CancellationToken {
        let background = &mut self.background;
        let service = self.global_search_service.get_or_init(|| {
            let (service, worker) = crate::app_search::LatestQueryService::channel();
            background.spawn_untracked("global-search-service", worker); service
        });
        let cancel = crate::background::CancellationToken::new();
        let job_cancel = cancel.clone();
        let activity = background.activity.start(crate::performance::worker_label("global-search"), None).cancellable(cancel.clone());
        service.replace(crate::app_search::LatestQueryJob { cancel: cancel.clone(), run: Box::new(move || {
            crate::worker_priority::lower_current_thread();
            let _activity = activity.running(); run(job_cancel);
        }) });
        cancel
    }
    fn reset_global_search_request(&mut self) {
        self.global_search_model_updates.clear();
        self.global_search_pending_since = None;
        if self.status_message.starts_with("Invalid regex:") { self.status_message.clear(); }
        if let Some(token) = self.search_cancel_token.take() { token.cancel(); }
        self.search_generation = self.search_generation.wrapping_add(1);
        self.global_search_results.clear();
        self.global_search_results.set_remote(self.remote_workspace.clone());
        self.global_search_truncated = false;
        self.global_search_stopped = false;
        self.global_search_error = None;
        self.global_search_selected = 0;
        self.global_search_selected_header = None;
        self.global_search_scroll = 0;
        self.bg_global_search_running = false;
    }

    /// Coalesce automatic workspace searches without holding a worker or taking
    /// document snapshots until typing settles. Stale replies are rejected now.
    pub(crate) fn schedule_global_search(&mut self) {
        self.schedule_global_search_at(Instant::now());
    }

    fn schedule_global_search_at(&mut self, now: Instant) {
        self.reset_global_search_request();
        if self.global_search_query.is_empty() || !self.has_open_folder() { return; }
        if self.search_debounce_ms == 0 {
            self.start_global_search();
        } else {
            self.global_search_pending_since = Some(now);
            self.bg_global_search_running = true;
        }
    }

    pub(crate) fn global_search_debounce_wait(&self, now: Instant) -> Option<Duration> {
        if !self.workspace_visible { return None; }
        let query = self.global_search_pending_since.map(|start| Duration::from_millis(self.search_debounce_ms)
            .saturating_sub(now.saturating_duration_since(start)));
        let model = self.global_search_model_updates.pending_since.map(|start| MODEL_SEARCH_QUIET_PERIOD
            .saturating_sub(now.saturating_duration_since(start)));
        query.into_iter().chain(model).min()
    }

    pub(crate) fn poll_pending_global_search(&mut self, now: Instant) -> bool {
        if !self.workspace_visible { return false; }
        let mut changed = false;
        if self.global_search_pending_since.is_some_and(|start|
            now.saturating_duration_since(start) >= Duration::from_millis(self.search_debounce_ms)) {
            self.global_search_pending_since = None;
            self.start_global_search();
            changed = true;
        }
        if self.global_search_model_updates.pending_since.is_some_and(|start|
            now.saturating_duration_since(start) >= MODEL_SEARCH_QUIET_PERIOD) {
            self.start_global_search_model_updates();
            changed = true;
        }
        changed
    }

    pub(crate) fn global_search_edit_snapshot(&self) -> Option<(u64, u64)> {
        if self.global_search_query.is_empty() { return None; }
        let tab = self.tabs.get(self.active_tab)?;
        if tab.kind != crate::app::TabKind::File
            || !self.global_search_results.tracks_file(tab.editor.file_path.as_deref()?) {
            return None;
        }
        Some((tab.editor.navigation_id, tab.editor.content_version))
    }

    pub(crate) fn refresh_global_search_after_edit(&mut self, previous: Option<(u64, u64)>) {
        if let Some((id, version)) = previous
            && self.global_search_edit_snapshot().is_some_and(|after| after.0 == id && after.1 != version) {
            self.queue_global_search_editor_update();
        }
    }

    pub(crate) fn queue_global_search_editor_update(&mut self) {
        if self.global_search_edit_snapshot().is_none() { return; }
        let editor = self.editor().unwrap();
        let path = editor.file_path.clone().unwrap();
        let rope = editor.rope.clone();
        self.queue_global_search_model_update(path, rope);
    }

    pub(crate) fn queue_global_search_model_update(&mut self, path: PathBuf, rope: ropey::Rope) {
        if self.global_search_query.is_empty()
            || !self.global_search_results.mark_file_edited(&path) { return; }
        let updates = &mut self.global_search_model_updates;
        if updates.pending.get(&path).is_some_and(|pending| pending.rope.is_instance(&rope)) { return; }
        updates.work.cancel();
        updates.serial = updates.serial.wrapping_add(1);
        updates.pending.insert(path, ModelUpdate { serial: updates.serial, rope });
        updates.pending_since = Some(Instant::now());
    }

    fn start_global_search_model_updates(&mut self) {
        let updates = &mut self.global_search_model_updates;
        updates.pending_since = None;
        let sources: Vec<_> = updates.pending.iter()
            .map(|(path, update)| (path.clone(), update.serial, update.rope.clone())).collect();
        if sources.is_empty() { return; }
        let query = self.global_search_query.clone();
        let case_sensitive = self.global_search_case_sensitive;
        let whole_word = self.global_search_whole_word;
        let is_regex = self.global_search_regex;
        let max_results = self.max_global_search_results;
        let generation = self.search_generation;
        let sender = self.background.sender.clone();
        let cancel = crate::background::CancellationToken::new();
        let worker_cancel = cancel.clone();
        let activity = self.background.activity.start("Updating search results", None).cancellable(cancel.clone());
        updates.work.replace(self.background.interactive_sender(), crate::background::latest::Job {
            cancel,
            run: Box::new(move || {
                let _activity = activity.running();
                let regex = SearchRegex::cached_editor(&query, case_sensitive, whole_word, is_regex);
                for (path, serial, rope) in sources {
                    if worker_cancel.is_cancelled() { return; }
                    let result = match &regex {
                        Ok(regex) => {
                            let mut results = Vec::new();
                            let mut truncated = false;
                            Self::search_file_rope(&path, &rope, GlobalSearchSpec {
                                query: &query, regex: Some(regex), include: "", exclude: "",
                                max_results, case_sensitive, open_buffers: None,
                            }, &mut results, &mut truncated, &worker_cancel);
                            Ok((results, truncated))
                        }
                        Err(error) => Err(crate::search_regex::error_summary(error)),
                    };
                    if worker_cancel.is_cancelled() { return; }
                    let _ = sender.send(crate::background::BackgroundResult::GlobalSearchFileResults {
                        path, serial, generation, result,
                    });
                }
            }),
        });
    }

    pub(crate) fn apply_global_search_file_results(&mut self, path: PathBuf, serial: u64, generation: u64,
        result: Result<(Vec<GlobalSearchResult>, bool), String>) -> bool {
        if generation != self.search_generation
            || self.global_search_model_updates.pending.get(&path).is_none_or(|pending| pending.serial != serial) {
            return false;
        }
        self.global_search_model_updates.pending.remove(&path);
        let (mut results, truncated) = match result {
            Ok(result) => result,
            Err(error) => { self.global_search_error = Some(error); return true; }
        };
        results.retain(|result| self.global_search_results.accepts_refreshed_result(result));
        let available = self.max_global_search_results.saturating_sub(
            self.global_search_results.len() - self.global_search_results.file_matches(&path));
        self.global_search_truncated |= truncated || results.len() > available;
        results.truncate(available);
        let selected = self.global_search_results.get(self.global_search_selected).cloned();
        let anchor = self.global_search_results.row_at(self.global_search_scroll).map(|row| {
            let (crate::search_results::SearchRow::Header(index) | crate::search_results::SearchRow::Result(index)) = row;
            (self.global_search_results[index].clone(), matches!(row, crate::search_results::SearchRow::Header(_)))
        });
        self.global_search_results.replace_file(&path, results);
        if let Some(index) = selected.as_ref().and_then(|result| self.global_search_results.index_of(result)) {
            self.global_search_selected = index;
        }
        self.ensure_global_search_selection_visible();
        if let Some((result, header)) = anchor
            && let Some(index) = self.global_search_results.index_of(&result)
            && let Some(row) = if header { self.global_search_results.header_row(index) }
                else { self.global_search_results.result_row(index) } {
            self.global_search_scroll = row;
        }
        let visible = self.explorer_area.map_or(1, |area| {
            usize::from(area.bottom().saturating_sub(crate::ui_search_panel::search_results_start_y(self, area)))
        }).max(1);
        self.global_search_scroll = self.global_search_scroll.min(self.global_search_results.row_count().saturating_sub(visible));
        true
    }

    /// Explicit refreshes bypass the typing delay.
    pub(crate) fn update_global_search(&mut self) {
        self.reset_global_search_request();
        self.start_global_search();
    }

    fn start_global_search(&mut self) {
        if !self.has_open_folder() {
            self.bg_global_search_running = false;
            self.global_search_pending_since = None;
            return;
        }
        if !self.workspace_visible {
            self.global_search_pending_since = (!self.global_search_query.is_empty()).then(Instant::now);
            if self.global_search_query.is_empty() { self.bg_global_search_running = false; }
            return;
        }
        self.bg_global_search_running = false;
        if self.global_search_query.is_empty() { return; }
        let query = self.global_search_query.clone();
        let case_sensitive = self.global_search_case_sensitive;
        let whole_word = self.global_search_whole_word;
        let is_regex = self.global_search_regex;
        let root = self.explorer.root.clone();
        let include = self.search_include_pattern.clone();
        let exclude = self.search_exclude_pattern.clone();
        let settings = self.settings_for_scope(crate::settings::SettingsScope::Workspace);
        let follow_links = settings.search_follow_symlinks.unwrap_or(true);
        let max_results = self.max_global_search_results;
        let open_buffers: OpenSearchBuffers = self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .filter(|tab| tab.kind == crate::app::TabKind::File)
            .filter_map(|tab| {
                tab.editor.refresh_document();
                if tab.editor.bg_loading { return None; }
                Some((tab.editor.file_path.clone()?, tab.editor.rope.clone()))
            })
            .collect();
        let sender = self.background.sender.clone();
        let generation = self.search_generation;
        self.bg_global_search_running = true;
        if let Some(remote) = self.remote_workspace.clone() {
            let request = crate::remote::search::Query {
                stream: true,
                report_open_aliases: true,
                text: query.clone(), case_sensitive, whole_word, regex: is_regex,
                include: include.clone(), exclude: exclude.clone(), limit: max_results,
                files_exclude: settings.files_exclude.clone(), search_exclude: settings.search_exclude.clone(),
                use_ignores: false, parent_ignores: false, global_ignores: false, follow_links,
                open_files: Vec::new(),
            };
            self.search_cancel_token = Some(self.submit_global_search(move |cancel| {
                if cancel.is_cancelled() { return; }
                // View keys are opaque client paths. Native include/exclude
                // semantics belong to the node, including absolute and ~/ paths.
                // Cache scope replies when the node used only path metadata.
                let mut paths = open_buffers.keys().cloned().collect::<Vec<_>>();
                paths.sort_unstable();
                let selected = match remote.search_scope_with_settings(&paths, &include, &exclude,
                    &request.files_exclude, &request.search_exclude, &cancel) {
                    Ok(indices) => indices,
                    Err(error) => {
                        if !cancel.is_cancelled() { search_failed(&sender, generation, error.to_string()); }
                        return;
                    }
                };
                if cancel.is_cancelled() { return; }
                let scan_drafts = |indices: &[usize], limit| {
                    let mut results = Vec::new();
                    let mut truncated = false;
                    if indices.is_empty() {
                        return Ok((results, truncated));
                    }
                    let regex = SearchRegex::cached_editor(&query, case_sensitive, whole_word, is_regex)
                        .map_err(|error| crate::search_regex::error_summary(&error))?;
                    let spec = GlobalSearchSpec {
                        query: &query,
                        regex: Some(&regex),
                        include: "",
                        exclude: "",
                        max_results: limit,
                        case_sensitive,
                        open_buffers: None,
                    };
                    for &index in indices {
                        if cancel.is_cancelled() || truncated {
                            break;
                        }
                        let path = &paths[index];
                        Self::search_file_rope(
                            path, &open_buffers[path], spec,
                            &mut results, &mut truncated, &cancel,
                        );
                    }
                    Ok::<_, String>((results, truncated))
                };
                let (local, truncated) = match scan_drafts(&selected, max_results) {
                    Ok(result) => result,
                    Err(error) => { search_failed(&sender, generation, error); return; }
                };
                if cancel.is_cancelled() { return; }
                let mut request = request;
                request.limit = max_results.saturating_sub(local.len());
                if truncated || request.limit == 0 {
                    let _ = sender.send(crate::background::BackgroundResult::GlobalSearchResults { results: local, done: true, generation, truncated }); return;
                }
                if !local.is_empty() {
                    let _ = sender.send(crate::background::BackgroundResult::GlobalSearchResults { results: local, done: false, generation, truncated: false });
                }
                let mut open_indices = Vec::new();
                for (index, path) in paths.iter().enumerate() {
                    if let Ok(remote_path) = remote.remote_path(path) {
                        request.open_files.push(remote_path);
                        open_indices.push(index);
                    }
                }
                let mut remaining = request.limit;
                let response = remote.client.search(request, &cancel, |hits| {
                        if cancel.is_cancelled() { return Err(std::io::ErrorKind::Interrupted.into()); }
                        remaining = remaining.saturating_sub(hits.len());
                        let results = hits.into_iter().map(|hit| crate::app::GlobalSearchResult {
                            file_path: remote.view_key(&hit.path), line_num: hit.line, line_text: hit.text,
                            col_start: hit.start, col_end: hit.end, context_before: None, context_after: None,
                        }).collect();
                        sender.send(crate::background::BackgroundResult::GlobalSearchResults { results, done: false, generation, truncated: false })
                            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Search result receiver closed"))
                });
                if cancel.is_cancelled() { return; }
                match response {
                    Ok((disk_truncated, draft_aliases)) => {
                        let aliases: Vec<_> = draft_aliases.into_iter()
                            .map(|index| open_indices[index])
                            .filter(|index| selected.binary_search(index).is_err())
                            .collect();
                        match scan_drafts(&aliases, remaining) {
                            Ok((results, draft_truncated)) if !cancel.is_cancelled() => {
                                let _ = sender.send(crate::background::BackgroundResult::GlobalSearchResults {
                                    results, done: true, generation,
                                    truncated: disk_truncated || draft_truncated,
                                });
                            }
                            Ok(_) => {}
                            Err(error) => search_failed(&sender, generation, error),
                        }
                    }
                    Err(error) => {
                        search_failed(&sender, generation, error.to_string());
                    }
                }
            }));
            return;
        }
        self.search_cancel_token = Some(self.submit_global_search(move |cancel| {
            if cancel.is_cancelled() { return; }
            let regex = match SearchRegex::workspace(&query, case_sensitive, whole_word, is_regex) {
                Ok(regex) => regex,
                Err(error) => { search_failed(&sender, generation, crate::search_regex::error_summary(&error)); return; }
            };
            let includes = crate::search_paths::SearchPaths::cached(&root, &include);
            let excludes = crate::search_paths::SearchPaths::cached(&root, &exclude);
            let setting_excludes = std::sync::Arc::new(crate::search_paths::ExcludeRules::new(
                &settings.files_exclude, &settings.search_exclude));
            if cancel.is_cancelled() { return; }
            let open_paths: Vec<_> = open_buffers.keys().map(PathBuf::as_path).collect();
            let open_aliases = OpenFileAliases::new(
                open_paths.iter().copied().enumerate(),
                &includes,
                follow_links,
                &cancel,
            );
            let path_aliases = std::sync::Arc::<crate::search_paths::PathAliases>::default();
            let disk_paths = crate::search_walk::paths(
                &root, includes.clone(), excludes.clone(), setting_excludes.clone(),
                crate::search_walk::Options {
                    follow_links,
                    workers: 1,
                    open_aliases: open_aliases.clone(),
                    path_aliases: path_aliases.clone(),
                },
                &cancel,
            );
            let mut results = Vec::new();
            let mut truncated = false;
            let mut sent = 0;
            let mut last_publish = std::time::Instant::now();
            let mut visited = rustc_hash::FxHashSet::default();
            let mut matched_files = follow_links.then(MatchedFiles::default);
            // Open models use TextModelSearch, bypass ignore files, and take
            // precedence over the disk copy even when they contain no matches.
            let mut paths = open_buffers.keys().cloned().map(|path| (path, true, false))
                .chain(disk_paths.map(|(path, is_symlink)| (path, false, is_symlink)));
            loop {
                if cancel.is_cancelled() { return; }
                let Some((path, from_open, is_symlink)) = paths.next() else { break; };
                if from_open {
                    let candidate = crate::search_paths::Candidate::new(&path);
                    let included = if has_parent_traversal(&path) {
                        let Ok(actual) = path.canonicalize() else { continue; };
                        let actual_root = root.canonicalize().unwrap_or_else(|_| root.clone());
                        let resolved = crate::search_paths::Candidate::new(&actual);
                        includes.includes_canonical_open(&resolved)
                            && !excludes.excludes_with_aliases(&resolved, &actual_root, &path_aliases)
                            && !setting_excludes.excludes_tree(&actual, &actual_root)
                    } else {
                        includes.includes_open_with_aliases(&candidate, &path_aliases)
                    };
                    if !included || excludes.excludes_with_aliases(&candidate, &root, &path_aliases)
                        || setting_excludes.excludes_tree(&path, &root) { continue; }
                }
                let (path, is_open) = if from_open || open_buffers.contains_key(&path) {
                    (path, true)
                } else if let Some(index) = open_aliases.as_ref().and_then(|aliases| aliases.get(&path)) {
                    (open_paths[index].to_path_buf(), true)
                } else {
                    (path, false)
                };
                if !visited.insert(path.clone()) { continue; }
                let spec = GlobalSearchSpec {
                    query: &query, regex: Some(&regex), include: "", exclude: "",
                    max_results: max_results.saturating_sub(sent), case_sensitive,
                    open_buffers: Some(&open_buffers),
                };
                let first_match = results.len();
                Self::search_single_file(&path, &root, spec, &mut results, &mut truncated, &cancel);
                if (results.len() > first_match || truncated)
                    && let Some(matched_files) = &mut matched_files
                    && !is_open
                    && !matched_files.insert(&path, is_symlink)
                {
                    results.truncate(first_match);
                    truncated = false;
                }
                if truncated { break; }
                if !results.is_empty() && (results.len() >= 32 || last_publish.elapsed() >= std::time::Duration::from_millis(80)) {
                    sent += results.len();
                    let _ = sender.send(crate::background::BackgroundResult::GlobalSearchResults {
                        results: std::mem::take(&mut results), done: false, generation, truncated: false,
                    });
                    last_publish = std::time::Instant::now();
                }
            }
            if !cancel.is_cancelled() {
                let _ = sender.send(crate::background::BackgroundResult::GlobalSearchResults { results, done: true, generation, truncated });
            }
        }));
    }
    pub(crate) fn stop_global_search(&mut self) {
        if !self.bg_global_search_running { return; }
        self.global_search_pending_since = None;
        if let Some(token) = self.search_cancel_token.take() { token.cancel(); }
        // Reject queued results and failures as well as cancelling new work.
        // Keep the query, results, selection, and scroll position intact.
        self.search_generation = self.search_generation.wrapping_add(1);
        self.bg_global_search_running = false;
        self.global_search_stopped = true;
    }

    pub(crate) fn clear_global_search(&mut self) {
        self.push_global_search_history();
        self.global_search_query.clear();
        self.global_search_field_cursors[0] = 0;
        self.global_search_field_selection_anchors[0] = None;
        self.global_search_selected = 0;
        self.global_search_scroll = 0;
        self.update_global_search();
    }

    pub(crate) fn dismiss_global_search_result(&mut self, row: crate::search_results::SearchRow) {
        use crate::search_results::SearchRow;
        let anchor = self.global_search_results.row_at(self.global_search_scroll).map(|row| {
            let (SearchRow::Header(index) | SearchRow::Result(index)) = row;
            (self.global_search_results[index].clone(), matches!(row, SearchRow::Header(_)))
        });
        let Some(removed) = self.global_search_results.dismiss(row) else { return; };
        if self.global_search_selected >= removed.end {
            self.global_search_selected -= removed.len();
        } else if self.global_search_selected >= removed.start {
            self.global_search_selected = removed.start;
        }
        self.ensure_global_search_selection_visible();
        if let Some((result, header)) = anchor
            && let Some(index) = self.global_search_results.index_of(&result)
            && let Some(row) = if header { self.global_search_results.header_row(index) }
                else { self.global_search_results.result_row(index) }
        {
            self.global_search_scroll = row;
        }
        let visible = self.explorer_area.map_or(1, |area| {
            usize::from(area.bottom().saturating_sub(crate::ui_search_panel::search_results_start_y(self, area)))
        }).max(1);
        self.global_search_scroll = self.global_search_scroll.min(self.global_search_results.row_count().saturating_sub(visible));
    }

    /// Search open models first; decode disk files with BOM detection.
    pub(crate) fn search_single_file(
        path: &Path,
        workspace_root: &Path,
        spec: GlobalSearchSpec<'_>,
        results: &mut Vec<GlobalSearchResult>,
        truncated: &mut bool,
        cancel: &crate::background::CancellationToken,
    ) {
        if cancel.is_cancelled() {
            return;
        }
        if !spec.include.trim().is_empty() || !spec.exclude.trim().is_empty() {
            let candidate = crate::search_paths::Candidate::new(
                path.strip_prefix(workspace_root).unwrap_or(path));
            if !spec.include.trim().is_empty() && !crate::search_paths::matches_glob_prepared(&candidate, spec.include) {
                return;
            }
            if !spec.exclude.trim().is_empty() && crate::search_paths::matches_glob_prepared(&candidate, spec.exclude) {
                return;
            }
        }
        if let Some(rope) = spec.open_buffers.and_then(|buffers| buffers.get(path)) {
            let editor_regex = spec.regex.map(SearchRegex::for_open_buffer);
            Self::search_file_rope(
                path,
                rope,
                GlobalSearchSpec {
                    regex: editor_regex.as_ref(),
                    ..spec
                },
                results,
                truncated,
                cancel,
            );
            return;
        }
        let fallback;
        let regex = if let Some(regex) = spec.regex { regex } else {
            fallback = match SearchRegex::workspace(spec.query, spec.case_sensitive, false, false) { Ok(regex) => regex, Err(_) => return };
            &fallback
        };
        if let Some((matches, limited)) = regex.scan_disk(path, spec.max_results.saturating_sub(results.len()), cancel) {
            *truncated |= limited;
            results.extend(matches.into_iter().map(|found| GlobalSearchResult {
                file_path: path.to_path_buf(), line_num: found.row + 1,
                line_text: found.preview, col_start: found.start, col_end: found.end,
                context_before: None, context_after: None,
            }));
            return;
        }
        if cancel.is_cancelled() { return; }
        let Ok(loaded) = crate::editor::load_text_file(path) else {
            return;
        };
        if loaded.encoding == "Binary" || cancel.is_cancelled() {
            return;
        }
        // ripgrep treats NUL as binary even beyond its initial read buffer.
        if loaded.rope.chunks().any(|chunk| chunk.contains('\0')) {
            return;
        }
        Self::search_file_rope(path, &loaded.rope, spec, results, truncated, cancel);
    }
    #[cfg(test)]
    fn search_file_content(
        path: &Path,
        content: &str,
        spec: GlobalSearchSpec<'_>,
        results: &mut Vec<GlobalSearchResult>,
        truncated: &mut bool,
        cancel: &crate::background::CancellationToken,
    ) {
        Self::search_file_rope(
            path,
            &ropey::Rope::from_str(content),
            spec,
            results,
            truncated,
            cancel,
        );
    }

    fn search_file_rope(
        path: &Path,
        rope: &ropey::Rope,
        spec: GlobalSearchSpec<'_>,
        results: &mut Vec<GlobalSearchResult>,
        truncated: &mut bool,
        cancel: &crate::background::CancellationToken,
    ) {
        let fallback;
        let regex = if let Some(regex) = spec.regex { regex } else {
            fallback = match SearchRegex::workspace(spec.query, spec.case_sensitive, false, false) { Ok(regex) => regex, Err(_) => return };
            &fallback
        };
        let (matches, limited) = regex.scan(
            rope,
            None,
            spec.max_results.saturating_sub(results.len()),
            cancel,
        );
        *truncated |= limited;
        let mut preview: Option<(usize, usize, usize, std::sync::Arc<str>)> = None;
        for (row, col_start, col_end) in matches {
            if cancel.is_cancelled() { return; }
            let first = rope.line_to_char(row);
            let matched_end = (first + col_end).min(rope.len_chars());
            let end_row = rope.char_to_line(matched_end.saturating_sub(1)).max(row);
            if preview.as_ref().is_none_or(|&(line, last, length, _)| line != row || last != end_row || col_end > length) {
                let end = rope.line_to_char((end_row + 1).min(rope.len_lines()));
                let text = rope.slice(first..end).to_string();
                // Retain raw matched EOLs for replacement captures. Renderers
                // turn them into single-row markers without losing the source.
                let matched_end_byte = rope.char_to_byte(matched_end) - rope.char_to_byte(first);
                let keep = text.trim_end_matches(['\r', '\n']).len().max(matched_end_byte).min(text.len());
                let text = &text[..keep];
                preview = Some((row, end_row, text.chars().count(), std::sync::Arc::from(text)));
            }
            results.push(GlobalSearchResult {
                file_path: path.to_path_buf(), line_num: row + 1,
                line_text: preview.as_ref().unwrap().3.clone(), col_start, col_end,
                context_before: None, context_after: None,
            });
        }
    }

    #[cfg(test)]
    fn search_files_recursive_bg_cancel(
        dir: &Path, workspace_root: &Path, spec: GlobalSearchSpec<'_>,
        results: &mut Vec<GlobalSearchResult>, cancel: &crate::background::CancellationToken, truncated: &mut bool,
    ) {
        for entry in ignore::WalkBuilder::new(dir).hidden(false).require_git(false)
            .filter_entry(|entry| !crate::formatters::is_temporary_path(entry.path())).build().filter_map(Result::ok) {
            if *truncated || cancel.is_cancelled() { return; }
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                Self::search_single_file(entry.path(), workspace_root, spec, results, truncated, cancel);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn matches_glob(name: &str, pattern: &str) -> bool {
        crate::search_paths::matches_glob(name, pattern)
    }

    #[cfg(test)]
    pub(crate) fn glob_match(name: &str, pattern: &str) -> bool {
        crate::search_paths::glob_match(name, pattern)
    }
}

#[cfg(test)]
mod tests {
    use crate::search_regex::SearchRegex;
    use super::{GlobalSearchSpec, next_global_match};
    use crate::app::App;

    #[test]
    fn root_and_nested_search_share_filters_columns_and_line_previews() {
        let workspace = tempfile::tempdir().unwrap();
        let root_file = workspace.path().join("root.txt");
        let nested = workspace.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let text = "é needle needle\r\nÉ NEEDLE\n";
        std::fs::write(&root_file, text).unwrap();
        std::fs::write(nested.join("child.txt"), text).unwrap();
        std::fs::write(nested.join("skip.bin"), text).unwrap();
        let formatter_copy = nested.join(".vtcode-format-test");
        std::fs::create_dir(&formatter_copy).unwrap();
        std::fs::write(formatter_copy.join("copy.txt"), text).unwrap();
        let cancel = crate::background::CancellationToken::new();
        for regex in [None, Some(SearchRegex::workspace("needle", false, false, true).unwrap())] {
            let spec = GlobalSearchSpec {
                query: "needle",
                regex: regex.as_ref(),
                include: "*.txt",
                exclude: "",
                max_results: 100,
                case_sensitive: false,
                open_buffers: None,
            };
            let mut direct = Vec::new();
            let mut recursive = Vec::new();
            let mut truncated = false;
            App::search_single_file(
                &root_file,
                workspace.path(),
                spec,
                &mut direct,
                &mut truncated,
                &cancel,
            );
            App::search_files_recursive_bg_cancel(
                &nested,
                workspace.path(),
                spec,
                &mut recursive,
                &cancel,
                &mut truncated,
            );
            for results in [&direct, &recursive] {
                let ranges: Vec<_> = results
                    .iter()
                    .map(|r| (r.line_num, r.col_start, r.col_end))
                    .collect();
                assert_eq!(ranges, vec![(1, 2, 8), (1, 9, 15), (2, 2, 8)]);
                assert_eq!(&*results[0].line_text, "é needle needle");
                assert!(std::sync::Arc::ptr_eq(
                    &results[0].line_text,
                    &results[1].line_text
                ));
                assert!(!std::sync::Arc::ptr_eq(
                    &results[1].line_text,
                    &results[2].line_text
                ));
            }
            assert!(!truncated);
        }
    }

    #[test]
    fn workspace_search_keeps_binary_exclude_and_result_limits_without_a_size_cap() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("fixture.txt");
        let cancel = crate::background::CancellationToken::new();
        let mut spec = GlobalSearchSpec {
            query: "needle",
            regex: None,
            include: "",
            exclude: "",
            max_results: 2,
            case_sensitive: true,
            open_buffers: None,
        };
        for bytes in [b"\0needle".as_slice()] {
            std::fs::write(&path, bytes).unwrap();
            let mut results = Vec::new();
            App::search_single_file(
                &path,
                workspace.path(),
                spec,
                &mut results,
                &mut false,
                &cancel,
            );
            assert!(results.is_empty());
        }
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(10 * 1024 * 1024 + 1).unwrap();
        drop(file);
        let mut results = Vec::new();
        App::search_single_file(
            &path,
            workspace.path(),
            spec,
            &mut results,
            &mut false,
            &cancel,
        );
        assert!(results.is_empty());
        std::fs::write(&path, b"\xffneedle").unwrap();
        App::search_single_file(&path, workspace.path(), spec, &mut results, &mut false, &cancel);
        assert_eq!(results.len(), 1, "invalid UTF-8 is decoded for text search");
        results.clear();
        std::fs::write(&path, format!("{}needle", "x".repeat(10 * 1024 * 1024))).unwrap();
        App::search_single_file(&path, workspace.path(), spec, &mut results, &mut false, &cancel);
        assert_eq!(results.len(), 1, "large text files remain searchable");
        results.clear();
        std::fs::write(&path, "needle needle needle").unwrap();
        spec.exclude = "fixture.txt";
        App::search_single_file(
            &path,
            workspace.path(),
            spec,
            &mut results,
            &mut false,
            &cancel,
        );
        assert!(results.is_empty());
        spec.exclude = "";
        let mut truncated = false;
        App::search_single_file(
            &path,
            workspace.path(),
            spec,
            &mut results,
            &mut truncated,
            &cancel,
        );
        assert_eq!(results.len(), 2);
        assert!(truncated);
        results.clear();
        cancel.cancel();
        App::search_single_file(
            &path,
            workspace.path(),
            spec,
            &mut results,
            &mut false,
            &cancel,
        );
        assert!(results.is_empty());
    }

    #[test]
    fn workspace_zero_width_regex_advances_over_unicode_and_line_end() {
        let regex = SearchRegex::workspace("(?:)", true, false, true).unwrap();
        let spec = GlobalSearchSpec {
            query: "(?:)",
            regex: Some(&regex),
            include: "",
            exclude: "",
            max_results: 100,
            case_sensitive: true,
            open_buffers: None,
        };
        let mut results = Vec::new();
        App::search_file_content(
            std::path::Path::new("fixture"),
            "éx",
            spec,
            &mut results,
            &mut false,
            &crate::background::CancellationToken::new(),
        );
        assert_eq!(
            results
                .iter()
                .map(|r| (r.col_start, r.col_end))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 1), (2, 2)]
        );
    }

    #[test]
    fn streamed_disk_search_matches_decoded_search_for_regex_encodings_and_limits() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("fixture.txt");
        let cancel = crate::background::CancellationToken::new();
        let text = "é😀 needle needle\r\nNEEDLE\r\n\r\nend needle";
        let mut fixtures = vec![text.as_bytes().to_vec(), b"\xef\xbb\xbf".iter().copied().chain(text.bytes()).collect(),
            b"\xffneedle\nneedle".to_vec(), b"needle\rneedle\r".to_vec(), Vec::new(),
            "é😀\n\r\n".as_bytes().to_vec(), "needle\n".repeat(30_000).into_bytes()];
        for little in [false, true] {
            let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            fixtures.push(bytes);
        }
        for bytes in fixtures {
            std::fs::write(&path, &bytes).unwrap();
            let loaded = crate::editor::load_text_file(&path).unwrap();
            for (query, is_regex, whole_word) in [
                ("needle", false, false), ("needle", false, true), ("é😀", false, false),
                ("^", true, false), ("$", true, false), ("(?:)", true, false),
                (r"(?<=é)😀", true, false), (r"needle(?= needle)", true, false),
                (r"\r", true, false), (r"needle\r?\nNEEDLE", true, false),
                ("�", false, false),
            ] {
                let regex = SearchRegex::workspace(query, false, whole_word, is_regex).unwrap();
                for limit in [0, 1, 3, 100] {
                    let spec = GlobalSearchSpec { query, regex: Some(&regex), include: "", exclude: "",
                        max_results: limit, case_sensitive: false, open_buffers: None };
                    let (mut actual, mut expected) = (Vec::new(), Vec::new());
                    let (mut actual_limited, mut expected_limited) = (false, false);
                    App::search_single_file(&path, workspace.path(), spec, &mut actual, &mut actual_limited, &cancel);
                    App::search_file_rope(&path, &loaded.rope, spec, &mut expected, &mut expected_limited, &cancel);
                    let rows = |results: &[crate::app::GlobalSearchResult]| results.iter().map(|found|
                        (found.line_num, found.col_start, found.col_end, found.line_text.clone())).collect::<Vec<_>>();
                    assert_eq!((rows(&actual), actual_limited), (rows(&expected), expected_limited),
                        "query={query:?} limit={limit} bytes={}", bytes.len());
                }
            }
        }
    }

    #[test]
    fn streamed_search_discards_matches_when_binary_data_follows_result_limit() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("late-binary.txt");
        let regex = SearchRegex::workspace("needle", true, false, false).unwrap();
        let cancel = crate::background::CancellationToken::new();
        let spec = GlobalSearchSpec { query: "needle", regex: Some(&regex), include: "", exclude: "",
            max_results: 1, case_sensitive: true, open_buffers: None };
        let text = format!("needle needle\n{}\0tail", "ordinary line\n".repeat(30_000));
        std::fs::write(&path, text).unwrap();
        let mut results = Vec::new();
        let mut truncated = false;
        App::search_single_file(&path, workspace.path(), spec, &mut results, &mut truncated, &cancel);
        assert!(results.is_empty());
        assert!(!truncated);
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_search_workspace_file() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("fixture.txt");
        for (name, content, expected) in [
            (
                "absent",
                "ordinary source code without a match\n".repeat(100_000),
                0,
            ),
            ("dense-unicode", "é needle ".repeat(2_000), 2_000),
        ] {
            std::fs::write(&path, content).unwrap();
            let spec = GlobalSearchSpec {
                query: "needle",
                regex: None,
                include: "",
                exclude: "",
                max_results: 10_000,
                case_sensitive: true,
                open_buffers: None,
            };
            let mut samples = Vec::new();
            for _ in 0..7 {
                let mut results = Vec::new();
                let mut truncated = false;
                let started = std::time::Instant::now();
                App::search_single_file(
                    &path,
                    workspace.path(),
                    spec,
                    &mut results,
                    &mut truncated,
                    &crate::background::CancellationToken::new(),
                );
                samples.push(started.elapsed());
                assert_eq!(results.len(), expected);
                assert!(!truncated);
                std::hint::black_box(results);
            }
            samples.sort();
            eprintln!("search_workspace_file {name} median={:?}", samples[3]);
        }
    }

    #[test]
    fn glob_match_exact() {
        assert!(App::glob_match("hello.rs", "hello.rs"));
        assert!(!App::glob_match("hello.rs", "hello.js"));
    }

    #[test]
    fn glob_match_star_wildcard() {
        assert!(App::glob_match("hello.rs", "*.rs"));
        assert!(!App::glob_match("hello.rs", "*.js"));
        assert!(App::glob_match("test", "*"));
    }

    #[test]
    fn glob_match_question_wildcard() {
        assert!(App::glob_match("a.rs", "?.rs"));
        assert!(!App::glob_match("ab.rs", "?.rs"));
    }

    #[test]
    fn glob_match_is_case_sensitive() {
        assert!(!App::glob_match("Hello.RS", "*.rs"));
        assert!(!App::glob_match("FILE.TXT", "file.txt"));
    }

    #[test]
    fn matches_glob_comma_separated() {
        assert!(App::matches_glob("main.rs", "*.rs, *.toml"));
        assert!(App::matches_glob("Cargo.toml", "*.rs, *.toml"));
        assert!(!App::matches_glob("readme.md", "*.rs, *.toml"));
    }

    #[test]
    fn matches_glob_empty_pattern() {
        assert!(!App::matches_glob("file.rs", ""));
        assert!(!App::matches_glob("file.rs", "  ,  "));
    }

    #[test]
    fn matches_glob_workspace_relative_directory() {
        assert!(App::matches_glob("src_cc/backend/compiler.py", "./src_cc"));
        assert!(!App::matches_glob(
            "src_cc_extra/backend/compiler.py",
            "./src_cc"
        ));
    }

    #[test]
    fn matches_glob_directory_name_at_any_depth() {
        assert!(App::matches_glob("src/generated/output.rs", "generated"));
        assert!(!App::matches_glob(
            "src/generated_code/output.rs",
            "generated"
        ));
    }

    #[test]
    fn matches_glob_normalizes_windows_separators() {
        assert!(App::matches_glob("src\\nested\\main.rs", "src\\**\\*.rs"));
    }

    #[test]
    fn double_star_prefix_also_matches_workspace_root_files() {
        assert!(App::matches_glob("main.rs", "**/*.rs"));
    }

    #[test]
    fn global_search_regex_returns_the_real_match_range() {
        let regex = SearchRegex::new(r"n\w+e", true, false).unwrap();
        let spec = GlobalSearchSpec {
            query: "n\\w+e",
            regex: Some(&regex),
            include: "",
            exclude: "",
            max_results: 100,
            case_sensitive: true,
            open_buffers: None,
        };
        assert_eq!(
            next_global_match(
                "a needle here",
                0,
                spec,
                &memchr::memmem::Finder::new(spec.query.as_bytes())
            ),
            Some((2, 8))
        );
    }

    #[test]
    fn global_search_whole_word_does_not_match_inside_identifier() {
        let regex = SearchRegex::workspace("cat", true, true, true).unwrap();
        let spec = GlobalSearchSpec {
            query: "cat",
            regex: Some(&regex),
            include: "",
            exclude: "",
            max_results: 100,
            case_sensitive: true,
            open_buffers: None,
        };
        assert_eq!(
            next_global_match(
                "concatenate cat",
                0,
                spec,
                &memchr::memmem::Finder::new(spec.query.as_bytes())
            ),
            Some((12, 15))
        );
    }

    #[test]
    fn global_search_completes_without_coordinator_polling() {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        for directory in ["one", "two", "three"] {
            let directory = workspace.path().join(directory);
            std::fs::create_dir_all(&directory).expect("search directory");
            std::fs::write(directory.join("match.txt"), "a needle here\n").expect("search fixture");
        }

        let mut app = App::new(workspace.path().to_path_buf());
        app.global_search_query = "needle".to_string();
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            !app.bg_global_search_running
        });

        assert!(!app.bg_global_search_running);
        assert_eq!(app.global_search_results.len(), 3);
    }

    #[test]
    fn workspace_search_uses_explicit_defaults_without_ignore_files_and_prefers_open_buffers() {
        let workspace = tempfile::tempdir().unwrap();
        for file in ["visible.txt", ".hidden/a.txt", "target/a.txt", "build/a.txt", "dist/a.txt", "ignored.txt", "ignoredDir/a.txt", "node_modules/a.txt", ".git/a.txt"] {
            let path = workspace.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "needle").unwrap();
        }
        std::fs::write(workspace.path().join(".gitignore"), "ignored.txt\nignoredDir/\n").unwrap();
        let mut app = App::new(workspace.path().to_path_buf());
        let mut editor = crate::editor::Editor::test_with_text("unsaved needle");
        editor.file_path = Some(workspace.path().join("ignored.txt"));
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.global_search_query = "needle".into();
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert_eq!(app.global_search_results.len(), 7);
        assert!(app.global_search_results.iter().any(|result| &*result.line_text == "unsaved needle"));
        app.search_exclude_pattern = "./ignoredDir, ./node_modules, ./.git".into();
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert_eq!(app.global_search_results.len(), 6);
        app.search_include_pattern = "./target".into();
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert_eq!(app.global_search_results.len(), 1);
        assert!(app.global_search_results[0].file_path.ends_with("target/a.txt"));
    }

    #[test]
    fn workspace_search_paths_can_leave_the_workspace_and_apply_explicit_excludes() {
        // The Windows sandbox's TEMP path contains braces, which are glob
        // operators in VS Code's absolute search paths as well.
        let parent = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let root = parent.path().join("workspace");
        let outside = parent.path().join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        for file in ["paired.js", "paired.ts", "unpaired.js", "skip.md"] { std::fs::write(outside.join(file), "needle").unwrap(); }
        let mut app = App::new(root);
        app.global_search_query = "needle".into();
        app.search_include_pattern = "../outside, *.js".into();
        app.search_exclude_pattern = "**/paired.js".into();
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert_eq!(app.global_search_results.len(), 1);
        assert_eq!(app.global_search_results[0].file_path, outside.join("unpaired.js"));
        app.search_include_pattern = format!("{}/*.ts", outside.display());
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert_eq!(app.global_search_results.len(), 1);
        assert_eq!(app.global_search_results[0].file_path, outside.join("paired.ts"));
    }

    #[test]
    fn vscode_open_buffers_use_editor_word_rules_while_disk_uses_ripgrep() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("disk.txt");
        std::fs::write(&path, "猫 cat").unwrap();
        let mut app = App::new(root.path().to_path_buf());
        let mut editor = crate::editor::Editor::test_with_text("猫 cat");
        editor.file_path = Some(root.path().join("open.txt"));
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.global_search_regex = true;
        app.global_search_query = r"\w+".into();
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);
        let disk = app.global_search_results.iter().filter(|result| result.file_path == path).count();
        assert_eq!(disk, 2);
        assert_eq!(app.global_search_results.len(), 3);
        app.global_search_query = r"[^\n]".into();
        app.update_global_search();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_global_search_running);
        assert!(!app.global_search_results.is_empty());
    }
}
