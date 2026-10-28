use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::SystemTime;

#[path = "background_latest.rs"]
pub(crate) mod latest;

#[path = "bracket_depth.rs"]
mod bracket_depth;
pub use bracket_depth::BracketDepthIndex;
pub(crate) use bracket_depth::MAX_BRACKET_LINE_CHARS;

/// Limit result application to a bounded amount of work between frames. A
/// search, extension, or watcher burst must not monopolize the UI thread.
const MAX_BACKGROUND_RESULTS_PER_POLL: usize = 32;
const BACKGROUND_RESULT_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);

/// Bound application time as well as message count: 32 syntax/search results
/// can take much longer on a slower CPU. Messages stay in the channel until
/// consumed, so yielding never clones results or loses their ordering/wakeup.
#[derive(Default)]
pub(crate) struct ResultBudget {
    started: Option<std::time::Instant>,
    count: usize,
    deadline: Option<std::time::Instant>,
}

impl ResultBudget {
    pub(crate) fn until(deadline: std::time::Instant) -> Self {
        Self { deadline: Some(deadline), ..Self::default() }
    }

    pub(crate) fn next(&mut self, workers: &mut BackgroundWorkers) -> Option<BackgroundResult> {
        self.next_at(workers, std::time::Instant::now())
    }

    fn next_at(&mut self, workers: &mut BackgroundWorkers, now: std::time::Instant) -> Option<BackgroundResult> {
        if self.count >= MAX_BACKGROUND_RESULTS_PER_POLL
            || (self.count > 0 && self.deadline.is_some_and(|deadline| now >= deadline))
            || self.started.is_some_and(|start| now.saturating_duration_since(start) >= BACKGROUND_RESULT_BUDGET)
        {
            return None;
        }
        let result = workers.next_result()?;
        self.started.get_or_insert(now);
        self.count += 1;
        Some(result)
    }
}

/// Edge-triggered wakeup shared by terminal input and asynchronous producers.
///
/// The UI records the current generation before draining any queues, then waits
/// until a producer increments it or a real timer expires. This avoids a fixed
/// idle poll interval without risking a lost wake between the readiness check
/// and the wait.
#[derive(Clone, Default)]
pub(crate) struct UiWake {
    generation: Arc<AtomicU64>,
    wait: Arc<(Mutex<()>, Condvar)>,
}

impl UiWake {
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn notify(&self) {
        // The predicate and transition into Condvar::wait must share this
        // mutex with producers. An atomic generation alone does not prevent a
        // notification between the waiter's last check and actually sleeping.
        let guard = self.wait.0.lock().unwrap_or_else(|error| error.into_inner());
        self.generation.fetch_add(1, Ordering::Release);
        drop(guard);
        self.wait.1.notify_all();
    }

    pub(crate) fn wait_for_change(
        &self,
        observed: u64,
        timeout: Option<std::time::Duration>,
    ) {
        if self.generation() != observed || timeout.is_some_and(|timeout| timeout.is_zero()) {
            return;
        }
        let guard = self.wait.0.lock().unwrap_or_else(|error| error.into_inner());
        if self.generation() == observed {
            if let Some(timeout) = timeout {
                let _ = self.wait.1.wait_timeout_while(guard, timeout, |_| {
                    self.generation() == observed
                });
            } else {
                drop(
                    self.wait
                        .1
                        .wait_while(guard, |_| self.generation() == observed)
                        .unwrap_or_else(|error| error.into_inner()),
                );
            }
        }
    }
}

/// Identifies the exact editor contents a background cache was computed for.
/// Keeping this richer than a content version prevents two tabs that happen to
/// have the same revision number from sharing stale analysis results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorAnalysisTarget {
    pub active_group: usize,
    pub active_tab: usize,
    pub file_path: Option<PathBuf>,
    pub content_version: u64,
    pub len_bytes: usize,
    pub modified: bool,
    pub disk_modified: Option<SystemTime>,
}

impl EditorAnalysisTarget {
    /// Whether two snapshots belong to the same editor surface. Content and
    /// filesystem metadata deliberately do not participate: render caches may
    /// keep their last value visible while a newer snapshot is recomputed.
    pub(crate) fn same_editor(&self, other: &Self) -> bool {
        self.active_group == other.active_group
            && self.active_tab == other.active_tab
            && self.file_path == other.file_path
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordOccurrenceTarget {
    pub editor: EditorAnalysisTarget,
    pub word: std::sync::Arc<str>,
}

impl WordOccurrenceTarget {
    pub(crate) fn same_render_anchor(&self, other: &Self) -> bool {
        self.editor.same_editor(&other.editor) && self.word == other.word
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BracketAnalysisTarget {
    pub editor: EditorAnalysisTarget,
    pub navigation_id: u64,
    pub first_line: usize,
    pub visible_lines: usize,
}

/// Identifies the cursor and language whose matching bracket/tag decorations
/// are being computed. These searches may span the whole document, so they
/// must never run while the terminal UI is drawing a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorMatchTarget {
    pub editor: EditorAnalysisTarget,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub language_name: String,
}

impl CursorMatchTarget {
    pub(crate) fn same_render_anchor(&self, other: &Self) -> bool {
        self.editor.same_editor(&other.editor)
            && self.cursor_line == other.cursor_line
            && self.cursor_col == other.cursor_col
            && self.language_name == other.language_name
    }
}

/// One scope opener used by sticky scroll. `parent` points to the nearest
/// earlier opener with lower indentation, so rendering follows only the scope
/// chain instead of rescanning from the viewport to the start of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StickyScopeLine {
    pub line: usize,
    pub parent: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemsFileTarget {
    pub navigation_id: u64,
    pub file_path: Option<PathBuf>,
    pub file_name: String,
    pub content_version: u64,
    pub len_bytes: usize,
    pub line_count: usize,
}

/// Display text plus the editor view that supplied this marker. Basenames are
/// labels, not identities: two open files can have the same name.
pub type ProblemItem = (String, usize, String, String, u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProblemsAnalysisTarget {
    pub active_group: usize,
    pub files: Vec<ProblemsFileTarget>,
}

/// Exact editor snapshot whose text is being materialized for an LSP
/// notification. Rope cloning is constant-time; flattening happens on a worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LspDocumentTarget {
    pub path: PathBuf,
    pub content_version: u64,
}

/// Exact editor layout requested for background word-wrap indexing. Fold
/// ranges participate because they change visual rows without changing text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrapLayoutTarget {
    pub editor_id: u64,
    pub content_version: u64,
    pub wrap_revision: u64,
    pub len_bytes: usize,
    pub wrap_width: usize,
    pub tab_size: usize,
    pub folded_ranges: Vec<(usize, usize)>,
}

/// Filesystem state captured by the watcher thread. Metadata can block on
/// network or sleeping drives, so the UI receives the result instead of
/// probing every path in a watcher burst itself.
#[derive(Clone, Debug)]
pub struct WorkspacePathChange {
    /// Start of the metadata read, used to reject observations from an in-flight rollback.
    pub observed_at: std::time::Instant,
    pub path: PathBuf,
    pub exists: bool,
    pub modified: Option<SystemTime>,
    pub len: Option<u64>,
}

/// Filesystem details shown by the Explorer hover tooltip. Capturing these on
/// a worker prevents a render from blocking on metadata or file reads.
#[derive(Clone, Debug)]
pub struct ExplorerHoverInfo {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub line_count: Option<usize>,
}

/// Repository state that affects per-line blame text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameCacheKey {
    pub file_path: PathBuf,
    pub head_id: String,
    pub disk_modified: Option<SystemTime>,
    pub disk_len: u64,
    pub line_count: usize,
}

/// Exact editor snapshot requested for an asynchronous Git comparison.
/// Results are applied only while this still matches the active editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitCompareTarget {
    pub generation: u64,
    pub file_path: PathBuf,
    pub content_version: u64,
    pub len_bytes: usize,
    pub line_count: usize,
    pub last_disk_modified: Option<SystemTime>,
}

/// Exact active-editor snapshot requested for an asynchronous diff refresh.
/// The group/tab identity prevents a result from one split being installed in
/// another editor that happens to have the same content version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRefreshTarget {
    pub generation: u64,
    pub active_group: usize,
    pub active_tab: usize,
    pub file_path: Option<PathBuf>,
    pub content_version: u64,
    pub len_bytes: usize,
    pub line_count: usize,
    pub last_disk_modified: Option<SystemTime>,
}

/// Fully prepared compare state. Diff calculation, row alignment, character
/// highlights, and gutter markers are all built on the worker so applying this
/// result does not introduce a long frame on the UI thread.
pub struct PreparedGitCompare {
    pub source_revisions: Option<crate::git_diff_sources::Revisions>,
    /// Index contents for a staged diff; otherwise the editor already holds the target.
    pub target_snapshot: Option<ropey::Rope>,
    /// Source Control addition with no file in the base revision, even an empty one.
    pub is_new_file: bool,
    pub base_content: Arc<str>,
    pub file_name: String,
    pub diff_lines: Vec<(char, String)>,
    pub render_cache: crate::app::DiffRenderCache,
    pub target_line_markers: Vec<char>,
}

/// Diff rows and render metadata prepared away from the terminal event loop.
pub struct PreparedDiffRefresh {
    pub base_content: Arc<str>,
    pub diff_lines: Vec<(char, String)>,
    pub render_cache: crate::app::DiffRenderCache,
    pub target_line_markers: Vec<char>,
}

/// Replacement output for an editor that stayed open while work happened on
/// a Rope snapshot. The UI applies it only if the source version still matches.
pub struct OpenFileReplaceResult {
    pub path: PathBuf,
    pub source_document_id: u64,
    pub source_content_version: u64,
    pub prepared: crate::formatters::Prepared,
    pub replacements: usize,
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
pub struct GitActionOutcome {
    pub message: String,
    pub refresh_explorer: bool,
    pub refresh_log: bool,
    pub refresh_line_status: bool,
    pub reload_all_clean_files: bool,
    /// Clear only the submitted message in its original workspace.
    pub(crate) clear_commit_message: Option<crate::app_git::GitCommitRequest>,
    pub(crate) stage_all_commit_request: Option<crate::app_git::GitCommitRequest>,
    pub leave_amend_mode: bool,
    pub close_branch_picker: bool,
}

#[derive(Default)]
pub struct FilesystemActionOutcome {
    pub open_file: Option<PathBuf>,
    pub finish_rename: Option<PathBuf>,
    pub message: String,
    pub renames: Vec<(PathBuf, PathBuf)>,
    pub deleted: Vec<PathBuf>,
    pub clear_explorer_clipboard: bool,
    pub warning: bool,
    pub check_deleted_paths: Vec<PathBuf>,
}

/// A single isolated diff-row highlight: (line_index, hash, styled_spans).
pub type FragmentHighlightEntry = (usize, u64, Vec<(crate::textmate::Style, String)>);

/// Messages sent from background workers to the main UI thread.
pub enum BackgroundResult {
    MergeConflicts { view: u64, index: Arc<crate::merge_conflicts::Index> },
    MergeComparison(crate::merge_conflict_ui::Comparison),
    SaveConflictDiff(crate::app_save_conflict::DiffResult),
    RemoteReconnectStarted { generation: u64, serial: u64, result: Result<crate::remote::launch::Setup, String> },
    RemoteReconnected { generation: u64, serial: u64, result: Result<crate::remote::workspace::Workspace, String> },
    RemoteReconnectFile { generation: u64, check: crate::app_remote_connection::FileCheck },
    AgentFileResolved { agent: crate::app_acp::AgentId, handle: vtcode::acp::ClientHandle, id: vtcode::acp::protocol::RequestId, ticket: u64, request: vtcode::acp::FileRequest, aliases: Result<Vec<(PathBuf, Option<PathBuf>)>, vtcode::acp::protocol::Error> },
    RemoteTransferProgress { generation: u64, path: PathBuf, saving: bool, completed: u64, total: u64 },
    RemoteWorkspaceOpened {
        request: u64,
        source: u64,
        result: Result<crate::remote::workspace::Workspace, String>,
    },
    RemoteTasks(Result<Vec<crate::app::TaskDefinition>,String>),
    RemoteHunkPeek(crate::remote::hunk::PeekResult),
    RemoteDiskChecksFailed { paths: Vec<PathBuf> },
    RemoteDiskChecksDone(CancellationToken),
    RemoteFileValidated { generation: u64, path: PathBuf, document: u64,
        baseline: crate::remote::workspace::Baseline,
        result: Result<crate::remote::workspace::Validation, String> },
    RemoteLocationResolved { id: u64, result: Result<crate::remote::workspace::Location, String> },
    RemoteLocationProgress { id: u64, completed: u64, total: u64 },
    WorkspaceResult { generation: u64, result: Box<BackgroundResult> },
    RemoteFileResult { checkpoint: Arc<crate::remote::Checkpoint>, result: Box<BackgroundResult> },
    RemoteWorkspaceError { operation: &'static str, error: String },
    DeletionProgress(CancellationToken),
    /// A read-only Git job belongs to one enabled session. Cancelling that
    /// session also invalidates results already waiting in the UI queue.
    SourceControlRead {
        cancel: CancellationToken,
        result: Box<BackgroundResult>,
    },
    /// Desktop launch completed without blocking input handling.
    ExternalOpenDone(Result<String, String>),
    /// Git branch label (e.g. "main ↑2"); views add their own icon.
    GitBranch(String),
    /// A repository-specific read must not update another project's view.
    RepositoryRead { root: PathBuf, generation: u64, result: Box<BackgroundResult> },
    GitRepositories {
        root: PathBuf,
        workspace_generation: u64,
        repository_generation: u64,
        discovery: crate::git_repositories::Discovery,
        scans: Vec<crate::git_repositories::Scan>,
    },
    /// Discovery finished; the worktree scan has not produced a change count yet.
    GitRepositoryFound {
        root: PathBuf,
        workspace_generation: u64,
        repository_generation: u64,
    },
    /// One bounded page from the current Git history traversal.
    GitLogPage(crate::git_history::HistoryPage),
    CommitDetails(crate::commit_details::DetailsResult),
    CommitDiff(crate::commit_details::DiffResult),
    /// Visible Explorer entries rebuilt away from the terminal event loop.
    ExplorerRefreshed {
        generation: u64,
        canonical_root: PathBuf,
        entries: Vec<crate::explorer::FileEntry>,
        matches: crate::explorer_find::Matches,
    },
    ExplorerRefreshFailed { generation: u64, error: String },
    /// Metadata and optional line count requested by an Explorer hover.
    ExplorerHoverInfo { generation: u64, info: Option<ExplorerHoverInfo> },
    /// Directory picker contents collected without blocking keyboard/mouse
    /// navigation on a slow filesystem.
    FolderBrowserEntries {
        path: PathBuf,
        generation: u64,
        entries: Vec<PathBuf>,
        modified: std::collections::HashMap<PathBuf, std::time::SystemTime>,
        locations: Vec<(String, PathBuf)>,
        error: Option<String>,
    },
    FolderBrowserCreated {
        path: PathBuf,
        generation: u64,
        error: Option<String>,
    },
    /// Breadcrumb siblings collected without pausing pointer input. The bool
    /// caches whether each entry is a directory for rendering and activation.
    BreadcrumbEntries {
        generation: u64,
        segment: usize,
        entries: Vec<(PathBuf, bool)>,
    },
    /// Result from a remote git operation (fetch/pull/push)
    GitRemoteOp {
        op: &'static str,
        stdout: String,
        stderr: String,
        success: bool,
    },
    /// A repository mutation completed outside the event loop.
    GitActionDone {
        action: &'static str,
        result: Result<GitActionOutcome, String>,
    },
    /// Local and remote branches collected without blocking the picker open.
    GitBranchesLoaded(Result<Vec<crate::app::GitBranchPickerItem>, String>),
    /// Potentially recursive Explorer mutation completed off-thread.
    FilesystemActionDone {
        action: &'static str,
        result: Result<FilesystemActionOutcome, String>,
    },
    /// Global search results (incremental batch)
    GlobalSearchResults {
        results: Vec<crate::app::GlobalSearchResult>,
        done: bool,
        generation: u64,
        truncated: bool,
    },
    /// Search failures obey the same query lifetime as result batches.
    GlobalSearchFailed { generation: u64, error: String },
    /// Matches for the active file prepared from a Rope snapshot. Find input
    /// can arrive on every keystroke, so scanning never runs on the UI thread.
    #[cfg_attr(test, allow(dead_code))]
    SearchMatches {
        owner: u64,
        generation: u64,
        target: EditorAnalysisTarget,
        matches: Vec<(usize, usize, usize)>,
        truncated: bool,
        error: Option<String>,
    },
    /// Replace-all output for the active editor.
    #[cfg_attr(test, allow(dead_code))]
    FileReplaceDone {
        generation: u64,
        target: EditorAnalysisTarget,
        source_document_id: u64,
        prepared: crate::formatters::Prepared,
        replacements: usize,
    },
    PastePrepared { serial: u64, result: Result<Box<crate::editor_paste::Prepared>, String> },
    ExitRecoverySaved { generation: u64, result: Result<(), String> },
    /// Local edits can be applied while a remote closed-file batch is pending.
    GlobalReplaceOpenPrepared {
        workspace_generation: u64,
        generation: u64,
        open_files: Vec<OpenFileReplaceResult>,
        failures: usize,
    },
    GlobalReplaceDone {
        workspace_generation: u64,
        generation: u64,
        outcome: crate::workspace_replace::Outcome,
        candidates: rustc_hash::FxHashSet<PathBuf>,
        error: Option<String>,
    },
    /// Versioned fallback symbols extracted from already-open buffers.
    SourceSymbols(crate::source_symbols::Reply),
    /// Quick-open file list with generation for debouncing
    QuickOpenFiles {
        files: Vec<(i64, PathBuf)>,
        generation: u64,
    },
    /// Composer @ suggestions use their own cancellation/generation state.
    ChatFileSuggestions {
        agent: u64,
        generation: u64,
        root: PathBuf,
        paths: Vec<PathBuf>,
    },
    ChatAttachmentsPrepared { agent: u64, serial: u64, result: Result<Vec<PathBuf>, String> },
    ChatPastePrepared { agent: u64, serial: u64, result: crate::app_chat_files::hosted::paste::Batch },
    ChatPromptPrepared { agent: u64, session: vtcode::acp::protocol::SessionId, serial: u64, result: Result<crate::app_chat_files::hosted::PreparedPrompt, String>, draft: Option<crate::app_chat_files::hosted::paste::ResolvedDraft> },
    /// Debounced file-watcher burst with content and structural changes.
    FilesChanged {
        root: PathBuf,
        changes: Vec<WorkspacePathChange>,
        /// File names may have changed; content-only events keep name indexes valid.
        structure_changed: bool,
        /// The watcher lost events or cannot identify the affected paths.
        rescan: bool,
        /// Repository metadata is intentionally excluded from ordinary path
        /// handling, but it still invalidates Git-derived UI state.
        git_metadata_changed: bool,
    },
    /// Whether the native watcher for the current workspace started cleanly.
    FileWatcherState { root: PathBuf, active: bool },
    /// Metadata-only disk check requested by focus/periodic maintenance.
    FileDiskState(WorkspacePathChange),
    /// Stateful, incremental TextMate document slice from a background worker.
    HighlightBatch(crate::highlight::HighlightBatch),
    /// Diff rows are isolated fragments rather than a coherent document.
    HighlightFragments {
        registry_generation: u64,
        file_path: PathBuf,
        theme_key: String,
        language: Option<String>,
        entries: Vec<FragmentHighlightEntry>,
    },
    /// Recovery content for an untitled editor, matched by its restore ID.
    SessionUntitledLoaded { restore: crate::session::SessionRestore },
    /// File content loaded in background
    FileLoaded {
        workspace_generation: u64,
        path: PathBuf,
        rope: ropey::Rope,
        encoding: &'static str,
        file_size: u64,
        last_disk_modified: Option<SystemTime>,
        /// Empty for an ordinary open; populated for matching session tabs
        /// whose recovery sidecars were hydrated by this same worker.
        session_restores: Vec<crate::session::SessionRestore>,
        /// The user explicitly chose the built-in viewer, so do not offer
        /// extension-specific open handlers after binary/text detection.
        skip_open_prompt: bool,
    },
    /// File content failed to load in background
    FileLoadFailed {
        workspace_generation: u64,
        path: PathBuf,
        error: String,
        session_restores: Vec<crate::session::SessionRestore>,
    },
    /// A clean open editor was reloaded from disk without blocking the UI.
    FileReloaded {
        workspace_generation: u64,
        path: PathBuf,
        targets: Vec<(u64, u64)>, // document identity and content version at dispatch
        result: Result<crate::editor::LoadedTextFile, String>,
    },
    /// Extension command completed in background
    ExtensionCommandDone {
        extension_index: usize,
        extension_id: String,
        result: Result<String, String>,
        runtime: crate::extensions::ExtensionRuntimeBox,
    },
    ExtensionViewEventDone {
        exit_action: bool,
        close_view: Option<String>,
        extension_index: usize,
        extension_id: String,
        result: Result<(), String>,
        runtime: crate::extensions::ExtensionRuntimeBox,
    },
    /// Periodic guest callbacks completed outside the event loop.
    ExtensionTickDone {
        extension_index: usize,
        result: Result<(), String>,
        runtime: crate::extensions::ExtensionRuntimeBox,
    },
    /// Periodic extension discovery completed outside the event loop.
    ExtensionsDiscovered {
        generation: u64,
        discovered: Vec<crate::extension_manifest::DiscoveredExtension>,
        notices: Vec<String>,
    },
    /// Initial activation, including Wasm compilation, completed off the UI.
    ExtensionsInitialized {
        generation: u64,
        manager: Box<crate::extensions::ExtensionManager>,
        notices: Vec<String>,
    },
    /// Themes explicitly installed into vtcode discovered after the first frame.
    ExternalThemesLoaded(Vec<crate::app_theme::ThemeFile>),
    ThemeInstalled(Result<Vec<crate::app_theme::ThemeFile>, String>),
    SyntaxInstalled {
        generation: u64,
        result: Result<crate::syntax_install::InstalledSyntax, String>,
    },
    ExtensionVersionsDiscovered {
        request: u64,
        result: Result<crate::extension_versions::ExtensionVersions, String>,
    },
    ExtensionInstalled {
        request: u64,
        result: Result<Vec<crate::extension_install::InstalledExtension>, String>,
    },
    ExtensionRecoveries(Vec<crate::extension_recovery::RecoveryRecord>),
    /// Installed cache validation/rebuild and custom grammar parsing completed.
    SyntaxAssetsLoaded {
        root: PathBuf,
        result: Result<Option<crate::highlight::LoadedSyntaxAssets>, String>,
    },
    /// Publish a usable compiled registry before validating all source assets.
    StartupSyntaxCacheLoaded {
        root: PathBuf,
        loaded: crate::highlight::LoadedSyntaxAssets,
    },
    /// Git line diff status computed in background
    GitLineDiff {
        file_path: PathBuf,
        status: crate::editor::PreparedGitLineStatus,
    },
    /// Git comparison state prepared entirely off the UI thread.
    GitCompare {
        target: GitCompareTarget,
        result: Result<PreparedGitCompare, String>,
    },
    /// Updated live Source Control comparisons, including tabs in other groups.
    SourceControlDiffs {
        root: PathBuf,
        generation: u64,
        updates: Vec<crate::diff_tabs::SourceControlDiffUpdate>,
    },
    /// Rebuilt active compare state after either side changed.
    DiffRefresh {
        target: DiffRefreshTarget,
        base_revision: crate::app::DiffBaseRevision,
        result: Result<PreparedDiffRefresh, String>,
    },
    /// Exact content-derived editor caches computed away from the UI thread.
    EditorAnalysis {
        target: EditorAnalysisTarget,
        errors: usize,
        warnings: usize,
        max_line_width: usize,
        word_count: usize,
        unsaved_diff: crate::editor::UnsavedDiff,
        sticky_scopes: Arc<[StickyScopeLine]>,
    },
    /// Cancellation acknowledges completion before the UI reuses the pending
    /// slot. Rapid input therefore cannot fill the CPU queue with replacements.
    EditorWorkCancelled {
        kind: EditorWorkKind,
        token: CancellationToken,
    },
    /// Automatic rich analysis exceeded the configured deadline. The file
    /// remains open and editable, but expensive optional features are paused.
    RichFeaturesTimedOut {
        target: EditorAnalysisTarget,
        feature: &'static str,
    },
    /// Document depth index and bounded viewport guides prepared off-thread.
    BracketAnalysis {
        target: BracketAnalysisTarget,
        depth_index: Arc<BracketDepthIndex>,
        starting_depth: i32,
        guides: Vec<(usize, usize, usize, i32)>,
    },
    /// Lexically confirmed enclosing scopes shared by every cursor position.
    StructuralScopes {
        request: crate::animation_structural_scope::StructuralScopeRequest,
        index: Option<Arc<crate::animation_structural_scope::StructuralScopeIndex>>,
    },
    /// Matching bracket and markup tag ranges prepared from a Rope snapshot.
    CursorMatches {
        target: CursorMatchTarget,
        bracket: Option<(usize, usize)>,
        tag: Option<(usize, usize, usize, usize, usize, usize)>,
    },
    /// TODO/FIXME/HACK/BUG/NOTE markers scanned from open editors on a worker.
    ProblemsAnalysis {
        target: ProblemsAnalysisTarget,
        items: Vec<ProblemItem>,
        error_count: usize,
        warning_count: usize,
        info_count: usize,
        file_count: usize,
    },
    /// Exact count for the word under the cursor.
    WordOccurrences {
        target: WordOccurrenceTarget,
        count: usize,
    },
    /// Completion candidates collected from rope snapshots on a worker.
    VtComplete {
        generation: u64,
        active_group: usize,
        active_tab: usize,
        document_id: u64,
        cursor_line: usize,
        cursor_col: usize,
        content_version: u64,
        source_rope: ropey::Rope,
        source_identity: crate::completion_source::Source,
        language: String,
        suggestions: Vec<String>,
        details: Vec<crate::app_vtcomplete::CompletionDetail>,
        work_outcome: crate::app_vtcomplete::VtCompleteWorkOutcome,
        deferred_candidates: u32,
    },
    /// Full semantic-token response decoded away from the event loop.
    LspSemanticTokens {
        target: EditorAnalysisTarget,
        tokens: Vec<crate::lsp::DecodedSemanticToken>,
    },
    /// Visual-row prefix tree prepared from a Rope snapshot. Installation is
    /// constant-time and stale versions are rejected by the UI.
    WrapLayout {
        target: WrapLayoutTarget,
        offsets: crate::editor_wrap::WrapRowOffsets,
    },
    /// Background file save completed (or failed)
    FileSaved {
        source_document_id: u64,
        workspace_generation: u64,
        path: PathBuf,
        error: Option<std::io::Error>,
        last_disk_modified: Option<SystemTime>,
        file_size: Option<u64>,
        source_content_version: u64,
        source_rope: ropey::Rope,
        previous_disk: Option<crate::editor::DiskSaveBaseline>,
        saved_encoding: &'static str,
        transformed_lines: Option<(usize, usize)>,
        disk_written: bool,
        saved_rope: Option<ropey::Rope>,
    },
    /// Git blame for one immutable editor snapshot, including local edits.
    BlameCache {
        navigation_id: u64,
        file_path: PathBuf,
        document: Arc<crate::git_blame::Document>,
    },
    BlameHistory {
        navigation_id: u64,
        generation: u64,
        result: Result<crate::git_blame::History, String>,
    },
    BlameSnapshot {
        navigation_id: u64,
        generation: u64,
        result: Result<crate::git_blame::Snapshot, String>,
    },
}

/// Independently coalesced editor jobs sharing the CPU pool.
#[derive(Clone, Copy)]
pub(crate) enum EditorWorkKind { Metrics, Brackets, Cursor }

/// Shared flag that background workers check to exit early when superseded.
#[derive(Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub(crate) fn from_flag(flag: Arc<AtomicBool>) -> Self { Self(flag) }
    pub(crate) fn flag(&self) -> Arc<AtomicBool> { self.0.clone() }
    pub(crate) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    /// Signal this token — all holders will see is_cancelled() == true.
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    /// Check whether this operation has been cancelled.
    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub(crate) fn same_task(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Handle to the background worker system.
type PoolTask = Box<dyn FnOnce() + Send>;
#[path = "background_queue.rs"]
mod queue;

struct QueuedTask {
    task: PoolTask,
    guard: PoolJobGuard,
}

impl QueuedTask {
    fn run(mut self) {
        self.guard.start();
        (self.task)();
    }
}
type FileWatcherMessage = Option<notify::Result<notify::Event>>;

fn background_pool_size(available_parallelism: usize) -> usize {
    // Syntax, diff, wrap, and analysis jobs are deliberately CPU-bound. Leave
    // one logical processor available to the terminal/input thread and cap
    // the pool so opening a file cannot saturate a many-core machine with
    // optional analysis before the first interaction is handled.
    available_parallelism.saturating_sub(1).clamp(1, 3)
}

fn save_pool_size(available_parallelism: usize) -> usize {
    available_parallelism.saturating_sub(1).clamp(1, 2)
}

#[derive(Clone)]
struct FileWatcherControl {
    sender: mpsc::Sender<FileWatcherMessage>,
    stopped: Arc<AtomicBool>,
}

impl FileWatcherControl {
    fn stop(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            // `None` shares the watcher's event channel, so cancellation wakes
            // an otherwise indefinitely blocked receiver immediately.
            let _ = self.sender.send(None);
        }
    }

    #[cfg(test)]
    fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

struct ActiveJobGuard(Arc<AtomicUsize>, UiWake);

#[derive(Default)]
struct PoolLoad {
    queued: AtomicUsize,
    running: AtomicUsize,
    live: AtomicUsize,
}

struct PoolThreadGuard(Arc<PoolLoad>);

impl Drop for PoolThreadGuard {
    fn drop(&mut self) { self.0.live.fetch_sub(1, Ordering::Relaxed); }
}

struct PoolJobGuard {
    _active: ActiveJobGuard,
    load: Arc<PoolLoad>,
    running: bool,
}

impl PoolJobGuard {
    fn start(&mut self) {
        self.load.running.fetch_add(1, Ordering::Relaxed);
        self.load.queued.fetch_sub(1, Ordering::Relaxed);
        self.running = true;
    }
}

impl Drop for PoolJobGuard {
    fn drop(&mut self) {
        // Captured at submission, so discarded queued jobs and failed sends
        // release their counts too, even if their closure never runs.
        if self.running { self.load.running.fetch_sub(1, Ordering::Relaxed); }
        else { self.load.queued.fetch_sub(1, Ordering::Relaxed); }
    }
}

impl Drop for ActiveJobGuard {
    fn drop(&mut self) {
        if self.0.fetch_sub(1, Ordering::Relaxed) == 1 {
            self.1.notify();
        }
    }
}

/// Cloneable pool handle that tracks both queued and executing jobs. The event
/// loop uses this to poll worker results promptly only while real work exists.
#[derive(Clone)]
pub(crate) struct BackgroundTaskSender {
    sender: queue::Sender<QueuedTask>,
    priority: queue::Priority,
    active_jobs: Arc<AtomicUsize>,
    load: Arc<PoolLoad>,
    wake: UiWake,
}

#[path = "background_io.rs"]
mod io;

impl BackgroundTaskSender {
    fn pool(name: &'static str, size: usize, active_jobs: Arc<AtomicUsize>, wake: UiWake) -> Self {
        let (sender, receiver) = queue::channel::<QueuedTask>();
        let load = Arc::new(PoolLoad { live: AtomicUsize::new(size), ..PoolLoad::default() });
        for i in 0..size {
            let rx = receiver.clone();
            let load = load.clone();
            thread::Builder::new().name(format!("{name}-{i}")).spawn(move || {
                let _thread_guard = PoolThreadGuard(load);
                crate::worker_priority::lower_current_thread();
                loop {
                    let Some(task) = rx.recv() else { break; };
                    task.run();
                }
            }).expect("failed to spawn pool thread");
        }
        Self { sender, priority: queue::Priority::Analysis, active_jobs, load, wake }
    }

    pub(crate) fn send(&self, task: PoolTask) -> Result<(), ()> {
        self.active_jobs.fetch_add(1, Ordering::Relaxed);
        self.load.queued.fetch_add(1, Ordering::Relaxed);
        let guard = PoolJobGuard {
            _active: ActiveJobGuard(Arc::clone(&self.active_jobs), self.wake.clone()),
            load: Arc::clone(&self.load), running: false,
        };
        self.sender.send(QueuedTask { task, guard }, self.priority).map_err(|_| ())
    }
}

/// Result sender with an exact queue count. `std::sync::mpsc` has no portable
/// readiness query, so without this the UI could sleep after a worker had
/// already published its answer.
#[derive(Clone)]
pub(crate) struct BackgroundResultSender {
    sender: mpsc::Sender<BackgroundResult>,
    pending_results: Arc<AtomicUsize>,
    wake: UiWake,
}

impl BackgroundResultSender {
    pub(crate) fn send(
        &self,
        result: BackgroundResult,
    ) -> Result<(), mpsc::SendError<BackgroundResult>> {
        self.pending_results.fetch_add(1, Ordering::Release);
        match self.sender.send(result) {
            Ok(()) => {
                self.wake.notify();
                Ok(())
            }
            Err(error) => {
                self.pending_results.fetch_sub(1, Ordering::AcqRel);
                Err(error)
            }
        }
    }
}

pub struct BackgroundWorkers {
    pub(crate) activity: crate::performance::ActivityMonitor,
    pub(crate) sender: BackgroundResultSender,
    pub receiver: mpsc::Receiver<BackgroundResult>,
    join_handles: Vec<(&'static str, Option<thread::JoinHandle<()>>)>,
    /// Fixed pool sender for submitting short-lived work items
    pool_sender: BackgroundTaskSender,
    /// Predictions can park for input without occupying syntax/navigation CPU
    /// slots. Lazily create one sleeping executor for this window.
    completion_pool_sender: Option<BackgroundTaskSender>,
    /// Lazily started and independent of syntax/diff work. Save All must not
    /// spawn an OS thread (and compete for memory/disk bandwidth) per document.
    save_pool_sender: Option<BackgroundTaskSender>,
    /// Serial long-lived asset/discovery work must not occupy the short editor
    /// jobs' pool, especially when only one CPU worker is available.
    maintenance_pool_sender: Option<BackgroundTaskSender>,
    /// Chat context may wait on SSH or a remote disk. Keep it out of editor
    /// CPU/read pools and bound it across sessions instead of spawning threads.
    context_pool_sender: Option<BackgroundTaskSender>,
    read_pool: Option<io::ReadPool>,
    prioritized_read_path: Option<PathBuf>,
    /// Number of queued or executing short-lived jobs. The long-lived file
    /// watcher is deliberately excluded.
    active_jobs: Arc<AtomicUsize>,
    pending_results: Arc<AtomicUsize>,
    wake: UiWake,
    /// Cancels the watcher for the previous workspace when a folder switch
    /// installs a new root.
    file_watcher_control: Option<FileWatcherControl>,
}

impl BackgroundWorkers {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_wake(UiWake::default())
    }

    pub(crate) fn with_wake(wake: UiWake) -> Self {
        let (result_tx, receiver) = mpsc::channel();
        let active_jobs = Arc::new(AtomicUsize::new(0));
        let pending_results = Arc::new(AtomicUsize::new(0));
        let pool_size = thread::available_parallelism()
            .map_or(1, |parallelism| background_pool_size(parallelism.get()));
        let pool_sender = BackgroundTaskSender::pool("worker-pool", pool_size, active_jobs.clone(), wake.clone());
        Self {
            activity: crate::performance::ActivityMonitor::new(wake.clone()),
            sender: BackgroundResultSender {
                sender: result_tx,
                pending_results: Arc::clone(&pending_results),
                wake: wake.clone(),
            },
            receiver,
            join_handles: Vec::new(),
            pool_sender,
            completion_pool_sender: None,
            save_pool_sender: None,
            maintenance_pool_sender: None,
            context_pool_sender: None,
            read_pool: None,
            prioritized_read_path: None,
            active_jobs,
            pending_results,
            wake,
            file_watcher_control: None,
        }
    }

    /// Optional CPU analysis. Use `interactive_sender` for visible feedback,
    /// `read_for_file` for filesystem work, and maintenance for slow assets.
    pub(crate) fn pool_sender(&self) -> BackgroundTaskSender {
        self.pool_sender.clone()
    }

    /// Visible syntax and navigation results bypass queued bulk
    /// analysis, with a bounded burst so analysis continues making progress.
    pub(crate) fn interactive_sender(&self) -> BackgroundTaskSender {
        BackgroundTaskSender { priority: queue::Priority::Interactive, ..self.pool_sender.clone() }
    }

    pub(crate) fn completion_sender(&mut self) -> BackgroundTaskSender {
        self.completion_pool_sender.get_or_insert_with(|| {
            BackgroundTaskSender::pool("vtcomplete-worker", 1, self.active_jobs.clone(), self.wake.clone())
        }).clone()
    }

    pub(crate) fn maintenance_sender(&mut self) -> BackgroundTaskSender {
        self.maintenance_pool_sender.get_or_insert_with(|| {
            BackgroundTaskSender::pool("asset-worker", 1, self.active_jobs.clone(), self.wake.clone())
        }).clone()
    }

    pub(crate) fn wake_handle(&self) -> UiWake {
        self.wake.clone()
    }

    /// Submit optional CPU analysis without creating a thread per request.
    /// Latency-sensitive editor results use `submit_interactive` instead.
    pub(crate) fn submit_cancellable(
        &self,
        f: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let _ = self.pool_sender.send(Box::new(move || f(worker_token)));
        token
    }

    /// Visible editor work uses the same bounded pool, ahead of queued scans.
    /// This does not preempt running work; jobs must still honor cancellation.
    pub(crate) fn submit_interactive(
        &self,
        f: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let _ = self.interactive_sender().send(Box::new(move || f(worker_token)));
        token
    }

    pub(crate) fn submit_context(
        &mut self,
        name: &'static str,
        f: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> CancellationToken {
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let activity = self
            .activity
            .start(crate::performance::worker_label(name), None)
            .cancellable(token.clone());
        let sender = self.context_sender();
        let _ = sender.send(Box::new(move || {
            let _activity = activity.running();
            if !worker_token.is_cancelled() {
                f(worker_token);
            }
        }));
        token
    }

    pub(crate) fn context_sender(&mut self) -> BackgroundTaskSender {
        self.context_pool_sender
            .get_or_insert_with(|| {
                BackgroundTaskSender::pool(
                    "context-io",
                    2,
                    self.active_jobs.clone(),
                    self.wake.clone(),
                )
            })
            .clone()
    }

    /// Spawn a named background task.
    pub(crate) fn spawn(&mut self, name: &'static str, f: impl FnOnce() + Send + 'static) {
        self.spawn_for_file(name, None, f);
    }

    pub(crate) fn spawn_for_file(
        &mut self,
        name: &'static str,
        path: Option<&std::path::Path>,
        f: impl FnOnce() + Send + 'static,
    ) {
        // Reap finished threads before spawning to keep handle list bounded
        self.reap_finished();
        self.active_jobs.fetch_add(1, Ordering::Relaxed);
        let active_jobs = Arc::clone(&self.active_jobs);
        let wake = self.wake.clone();
        let activity = self.activity.start(crate::performance::worker_label(name), path);
        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                let _active_guard = ActiveJobGuard(active_jobs, wake);
                let _activity = activity.running();
                f();
            })
            .expect("failed to spawn background thread");
        self.join_handles.push((name, Some(handle)));
    }

    pub(crate) fn save_for_file(&mut self, path: &std::path::Path, f: impl FnOnce() + Send + 'static) {
        let pool = self.save_pool_sender.get_or_insert_with(|| {
            let size = thread::available_parallelism().map_or(1, |n| save_pool_size(n.get()));
            BackgroundTaskSender::pool("save-pool", size, self.active_jobs.clone(), self.wake.clone())
        });
        let activity = self.activity.start(crate::performance::worker_label("bg-save"), Some(path));
        pool.send(Box::new(move || {
            let _activity = activity.running();
            f();
        })).expect("save worker pool disconnected");
    }

    pub(crate) fn read_for_file(
        &mut self, name: &'static str, path: &std::path::Path, foreground: bool,
        f: impl FnOnce() + Send + 'static,
    ) {
        if self.prioritized_read_path.as_deref() == Some(path) {
            self.prioritized_read_path = None;
        }
        let pool = self.read_pool.get_or_insert_with(|| {
            let size = thread::available_parallelism().map_or(1, |n| save_pool_size(n.get()));
            io::ReadPool::new(size, self.active_jobs.clone(), self.wake.clone())
        });
        let activity = self.activity.start(crate::performance::worker_label(name), Some(path));
        pool.send_for_path(path, foreground, Box::new(move || {
            let _activity = activity.running();
            f();
        }));
    }

    pub(crate) fn prioritize_file_read(&mut self, path: &std::path::Path) {
        // Scan the pending recovery queue once per focused loading file, not
        // once per frame while the same slow read is still running.
        if self.prioritized_read_path.as_deref() == Some(path) { return; }
        self.prioritized_read_path = Some(path.to_path_buf());
        if let Some(pool) = &self.read_pool { pool.prioritize(path); }
    }

    #[cfg(test)]
    pub(crate) fn use_single_read_worker(&mut self) {
        assert!(self.read_pool.is_none());
        self.read_pool = Some(io::ReadPool::new(1, self.active_jobs.clone(), self.wake.clone()));
    }

    #[cfg(test)]
    pub(crate) fn use_single_cpu_worker(&mut self) {
        assert_eq!(self.active_job_count(), 0);
        self.pool_sender = BackgroundTaskSender::pool("single-test-worker", 1, self.active_jobs.clone(), self.wake.clone());
    }

    /// Spawn a named background task with a cancellation token.
    /// Returns the token so the caller can cancel the task later.
    pub(crate) fn spawn_cancellable(
        &mut self,
        name: &'static str,
        f: impl FnOnce(CancellationToken) + Send + 'static,
    ) -> CancellationToken {
        // Reap finished threads before spawning to keep handle list bounded
        self.reap_finished();
        let token = CancellationToken::new();
        let token_clone = token.clone();
        self.active_jobs.fetch_add(1, Ordering::Relaxed);
        let active_jobs = Arc::clone(&self.active_jobs);
        let wake = self.wake.clone();
        let activity = self.activity.start(crate::performance::worker_label(name), None);
        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                let _active_guard = ActiveJobGuard(active_jobs, wake);
                let _activity = activity.cancellable(token_clone.clone()).running();
                f(token_clone);
            })
            .expect("failed to spawn background thread");
        self.join_handles.push((name, Some(handle)));
        token
    }

    /// Start a service that sleeps between requests. Its owner supplies the
    /// shutdown signal; it does not count as a continuously active UI job.
    pub(crate) fn spawn_untracked(&mut self, name: &'static str, f: impl FnOnce() + Send + 'static) {
        self.reap_finished();
        let handle = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                f();
            })
            .expect("failed to spawn background thread");
        self.join_handles.push((name, Some(handle)));
    }

    /// Taking one message allocates nothing and does not move a pending burst
    /// into a temporary Vec on every UI iteration.
    fn next_result(&mut self) -> Option<BackgroundResult> {
        let result = self.receiver.try_recv().ok()?;
        self.pending_results.fetch_sub(1, Ordering::AcqRel);
        Some(result)
    }

    /// Tests can drain a count-bounded batch without a wall-clock dependency.
    #[cfg(test)]
    pub(crate) fn poll(&mut self) -> Vec<BackgroundResult> {
        let mut results = Vec::new();
        while results.len() < MAX_BACKGROUND_RESULTS_PER_POLL {
            let Some(result) = self.next_result() else {
                break;
            };
            results.push(result);
        }
        results
    }

    pub(crate) fn has_pending_results(&self) -> bool {
        self.pending_results.load(Ordering::Acquire) > 0
    }

    #[cfg(test)]
    pub(crate) fn has_active_jobs(&self) -> bool {
        self.active_job_count() > 0
    }

    pub(crate) fn active_job_count(&self) -> usize {
        self.active_jobs.load(Ordering::Relaxed)
    }

    /// Momentary queue counters only; this does not inspect OS threads or wait
    /// for workers. Counts can change independently while a snapshot is drawn.
    pub(crate) fn queue_snapshot(&self) -> (usize, usize, usize, usize) {
        let mut queued = 0;
        let mut running = 0;
        let mut live = 0;
        for pool in std::iter::once(&self.pool_sender).chain(self.save_pool_sender.as_ref())
            .chain(self.completion_pool_sender.as_ref())
            .chain(self.maintenance_pool_sender.as_ref()).chain(self.context_pool_sender.as_ref()) {
            queued += pool.load.queued.load(Ordering::Relaxed);
            running += pool.load.running.load(Ordering::Relaxed);
            live += pool.load.live.load(Ordering::Relaxed);
        }
        if let Some(pool) = &self.read_pool {
            queued += pool.load.queued.load(Ordering::Relaxed);
            running += pool.load.running.load(Ordering::Relaxed);
            live += pool.load.live.load(Ordering::Relaxed);
        }
        (queued, running, live, self.pending_results.load(Ordering::Relaxed))
    }

    pub(crate) fn refresh_activity(&mut self, now: std::time::Instant) {
        self.activity.refresh(now, self.active_job_count());
    }

    /// Clean up finished join handles (non-blocking).
    pub(crate) fn reap_finished(&mut self) {
        self.join_handles.retain_mut(|(_, handle)| {
            if let Some(h) = handle {
                if h.is_finished() {
                    if let Some(h) = handle.take() {
                        let _ = h.join(); // Error OK: thread may have panicked
                    }
                    false
                } else {
                    true
                }
            } else {
                false
            }
        });
    }

    /// Start a file-system watcher on the given directory.
    /// Events are filtered, deduplicated, and sent to the UI as one debounced burst.
    pub(crate) fn start_file_watcher(&mut self, root: PathBuf) {
        if let Some(control) = self.file_watcher_control.take() {
            control.stop();
        }
        let sender = self.sender.clone();
        let (watcher_tx, watcher_rx) = std::sync::mpsc::channel::<FileWatcherMessage>();
        let control = FileWatcherControl {
            sender: watcher_tx.clone(),
            stopped: Arc::new(AtomicBool::new(false)),
        };
        let stopped = Arc::clone(&control.stopped);
        self.spawn_untracked("file-watcher", move || {
            use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
            use std::time::Duration;

            let event_tx = watcher_tx.clone();
            let Ok(mut watcher) = RecommendedWatcher::new(
                move |event| {
                    let _ = event_tx.send(Some(event));
                },
                Config::default(),
            ) else {
                stopped.store(true, Ordering::Release);
                let _ = sender.send(BackgroundResult::FileWatcherState {
                    root: root.clone(),
                    active: false,
                });
                return;
            };
            if watcher.watch(&root, RecursiveMode::Recursive).is_err() {
                stopped.store(true, Ordering::Release);
                let _ = sender.send(BackgroundResult::FileWatcherState {
                    root: root.clone(),
                    active: false,
                });
                return;
            }
            let _ = sender.send(BackgroundResult::FileWatcherState {
                root: root.clone(),
                active: true,
            });

            // Block until a native event or explicit stop arrives. The timed
            // receive below exists only inside an active event burst, never
            // while the workspace is idle.
            let mut running = true;
            while running {
                match watcher_rx.recv() {
                    Ok(Some(event)) => {
                        let mut burst = WorkspaceEventBurst::default();
                        burst.push(&root, event);
                        if burst.is_empty() { continue; }
                        // Drain any more events that arrived within 300ms
                        let deadline = std::time::Instant::now() + Duration::from_millis(300);
                        loop {
                            let remaining =
                                deadline.saturating_duration_since(std::time::Instant::now());
                            if remaining.is_zero() {
                                break;
                            }
                            match watcher_rx.recv_timeout(remaining) {
                                Ok(Some(evt)) => burst.push(&root, evt),
                                Ok(None) => {
                                    running = false;
                                    break;
                                }
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                    running = false;
                                    break;
                                }
                            }
                        }
                        if !running {
                            break;
                        }
                        if burst.watcher_failed {
                            // An errored backend may stop delivering changes.
                            // Keep receiving events, but restore polling/expiry
                            // rather than trusting snapshots indefinitely.
                            let _ = sender.send(BackgroundResult::FileWatcherState {
                                root: root.clone(), active: false,
                            });
                        }
                        let paths = coalesce_workspace_event_paths(&root, burst.paths);
                        if !paths.is_empty() || burst.git_metadata_changed || burst.rescan {
                            let changes = paths.into_iter().map(inspect_workspace_path).collect();
                            let _ = sender.send(BackgroundResult::FilesChanged {
                                root: root.clone(),
                                changes,
                                structure_changed: burst.structure_changed,
                                rescan: burst.rescan,
                                git_metadata_changed: burst.git_metadata_changed || burst.rescan,
                            });
                            // Error OK: receiver dropped means no listener
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            stopped.store(true, Ordering::Release);
            let _ = sender.send(BackgroundResult::FileWatcherState {
                root,
                active: false,
            });
        });
        self.file_watcher_control = Some(control);
    }
}

impl Drop for BackgroundWorkers {
    fn drop(&mut self) {
        if let Some(control) = self.file_watcher_control.take() {
            control.stop();
        }
    }
}

/// Classify before probing metadata. Reads from the editor, Git, indexers, or
/// other users must not themselves trigger another round of workspace reads.
#[derive(Default)]
pub(crate) struct WorkspaceEventBurst {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) structure_changed: bool,
    pub(crate) git_metadata_changed: bool,
    pub(crate) rescan: bool,
    pub(crate) watcher_failed: bool,
}

impl WorkspaceEventBurst {
    pub(crate) fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.git_metadata_changed && !self.rescan
    }

    pub(crate) fn push(&mut self, root: &std::path::Path, event: notify::Result<notify::Event>) {
        use notify::event::{AccessKind, AccessMode, MetadataKind, ModifyKind};
        use notify::EventKind;
        let Ok(event) = event else {
            self.rescan = true;
            self.watcher_failed = true;
            return;
        };
        self.rescan |= event.need_rescan();
        let structural = match event.kind {
            EventKind::Access(AccessKind::Close(AccessMode::Write)) => false,
            EventKind::Access(_) | EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => return,
            EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_)) => false,
            _ => true,
        };
        self.rescan |= event.paths.is_empty();
        for path in event.paths {
            if crate::formatters::is_temporary_path(&path) || crate::editor::is_save_temporary_path(&path) {
                continue;
            }
            if is_git_metadata_event(root, &path) {
                self.git_metadata_changed = true;
                if structural && path.file_name().is_some_and(|name| name == ".git") {
                    self.structure_changed = true;
                    self.paths.push(path);
                }
            } else if !is_ignored_workspace_event(root, &path)
                || (structural && path.parent().is_some_and(|parent| workspace_path_is_watched(root, parent)))
            {
                self.structure_changed |= structural;
                self.paths.push(path);
            }
        }
    }
}

/// A tooltip must not stream a multi-gigabyte file from shared storage. A hard
/// read limit also handles files that grow after their metadata was inspected.
pub(crate) const HOVER_LINE_COUNT_MAX_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn bounded_hover_line_count(
    reader: impl std::io::Read, cancel: &CancellationToken,
) -> Option<usize> {
    use std::io::Read;
    let mut reader = reader.take(HOVER_LINE_COUNT_MAX_BYTES + 1);
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    let mut newlines = 0_usize;
    loop {
        if cancel.is_cancelled() { return None; }
        let read = reader.read(&mut buffer).ok()?;
        if read == 0 { return Some(newlines + 1); }
        bytes += read as u64;
        if bytes > HOVER_LINE_COUNT_MAX_BYTES { return None; }
        newlines += buffer[..read].iter().filter(|byte| **byte == b'\n').count();
    }
}

/// Inspect one path outside the UI thread. Kept here so watcher and periodic
/// disk checks share identical existence/mtime/length semantics.
pub(crate) fn inspect_workspace_path(path: PathBuf) -> WorkspacePathChange {
    let observed_at = std::time::Instant::now();
    let metadata = std::fs::metadata(&path);
    // Inaccessibility is not evidence of deletion. Missing-file observations
    // may close editors, so only confirmed absence is reported as missing.
    let exists = metadata.as_ref().map_or_else(
        |error| {
            !matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            )
        },
        |_| true,
    );
    let metadata = metadata.ok();
    WorkspacePathChange {
        observed_at,
        exists,
        modified: metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()),
        len: metadata.as_ref().map(std::fs::Metadata::len),
        path,
    }
}

/// Git's private storage is handled by Git metadata invalidation. Project paths
/// remain eligible regardless of directory names or search exclusion settings.
fn is_ignored_workspace_event(root: &std::path::Path, path: &std::path::Path) -> bool {
    if crate::formatters::is_temporary_path(path) || crate::editor::is_save_temporary_path(path) {
        return true;
    }
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative.components().any(|component| component.as_os_str() == ".git")
}

fn is_git_metadata_event(root: &std::path::Path, path: &std::path::Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        let mut components = relative.components();
        if !components.by_ref().any(|component| component.as_os_str() == ".git") {
            return false;
        }
        // Object downloads and lock-file preparation do not change visible Git
        // state. The subsequent index/ref/HEAD update is the invalidation.
        if components.next().is_some_and(|component| matches!(
            component.as_os_str().to_str(), Some("objects" | "logs" | "COMMIT_EDITMSG")
        )) {
            return false;
        }
        !path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(".lock"))
    })
}

pub(crate) fn workspace_path_is_watched(root: &std::path::Path, path: &std::path::Path) -> bool {
    path.starts_with(root) && !is_ignored_workspace_event(root, path)
}

pub(crate) fn coalesce_workspace_event_paths(root: &std::path::Path, mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.retain(|path| !crate::formatters::is_temporary_path(path) && !crate::editor::is_save_temporary_path(path)
        && (!is_ignored_workspace_event(root, path)
            || path.parent().is_some_and(|parent| workspace_path_is_watched(root, parent))));
    paths.sort_unstable();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_context_io_is_bounded_and_cannot_occupy_editor_read_save_or_cpu_workers() {
        use std::time::Duration;
        let mut workers = BackgroundWorkers::new();
        assert!(workers.context_pool_sender.is_none(), "no context threads at startup");
        let (started, running) = mpsc::channel();
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        for _ in 0..2 {
            let started = started.clone();
            let gate = gate.clone();
            workers.submit_context("chat-paste", move |_| {
                let _ = started.send(());
                let guard = gate.0.lock().unwrap();
                drop(gate.1.wait_while(guard, |ready| !*ready).unwrap());
            });
        }
        let timeout = Duration::from_secs(5);
        for _ in 0..2 { running.recv_timeout(timeout).unwrap(); }
        for _ in 0..64 {
            workers.submit_context("chat-paste", |_| panic!("cancelled context job ran")).cancel();
        }
        let (completed, results) = mpsc::channel();
        let cpu = completed.clone();
        workers.submit_interactive(move |_| { let _ = cpu.send("cpu"); });
        let read = completed.clone();
        workers.read_for_file("file-load", std::path::Path::new("fixture"), true, move || { let _ = read.send("read"); });
        workers.save_for_file(std::path::Path::new("fixture"), move || { let _ = completed.send("save"); });
        let independent: Vec<_> = (0..3).map(|_| results.recv_timeout(timeout)).collect();
        let pool = workers.context_pool_sender.as_ref().unwrap().load.clone();
        let active = pool.running.load(Ordering::Relaxed);
        let queued = pool.queued.load(Ordering::Relaxed);
        // Release readers before asserting, including on a regression path.
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(independent.iter().all(Result::is_ok), "{independent:?}");
        assert_eq!(active, 2);
        assert_eq!(queued, 64);
        let end = std::time::Instant::now() + timeout;
        while pool.running.load(Ordering::Relaxed) + pool.queued.load(Ordering::Relaxed) > 0 {
            assert!(std::time::Instant::now() < end, "cancelled context queue did not drain");
            std::thread::yield_now();
        }
    }

    #[test]
    fn watcher_read_access_does_not_trigger_any_workspace_io() {
        use notify::event::{AccessKind, AccessMode, MetadataKind, ModifyKind};
        use notify::EventKind;
        let root = std::path::Path::new("workspace");
        let mut burst = WorkspaceEventBurst::default();
        for kind in [
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Open(AccessMode::Read)),
            EventKind::Access(AccessKind::Open(AccessMode::Write)),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)),
        ] {
            for path in [root.join("main.rs"), root.join(".git/index")] {
                burst.push(root, Ok(notify::Event::new(kind).add_path(path)));
            }
        }
        assert!(burst.is_empty());
    }

    #[test]
    fn watcher_preserves_content_writes_without_invalidating_names() {
        use notify::event::{AccessKind, AccessMode, DataChange, ModifyKind};
        use notify::EventKind;
        let root = std::path::Path::new("workspace");
        let mut burst = WorkspaceEventBurst::default();
        for kind in [EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            EventKind::Access(AccessKind::Close(AccessMode::Write))]
        {
            burst.push(root, Ok(notify::Event::new(kind).add_path(root.join("main.rs"))));
        }
        assert!(!burst.is_empty());
        assert!(!burst.structure_changed);
        assert!(!burst.rescan);
        assert_eq!(coalesce_workspace_event_paths(root, burst.paths), [root.join("main.rs")]);
    }

    #[test]
    fn watcher_renames_and_nested_project_directory_changes_preserve_all_paths() {
        use notify::event::{CreateKind, ModifyKind, RenameMode};
        use notify::EventKind;
        let root = std::path::Path::new("workspace");
        let mut burst = WorkspaceEventBurst::default();
        burst.push(root, Ok(notify::Event::new(EventKind::Create(CreateKind::File))
            .add_path(root.join("target/deep/generated.rs"))));
        assert!(!burst.is_empty());
        burst.push(root, Ok(notify::Event::new(EventKind::Create(CreateKind::Folder)).add_path(root.join("target"))));
        burst.push(root, Ok(notify::Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(root.join("old.rs")).add_path(root.join("new.rs"))));
        assert!(burst.structure_changed);
        assert_eq!(coalesce_workspace_event_paths(root, burst.paths),
            [root.join("new.rs"), root.join("old.rs"), root.join("target"), root.join("target/deep/generated.rs")]);
    }

    #[test]
    fn watcher_overflow_and_errors_force_revalidation() {
        let root = std::path::Path::new("workspace");
        let mut burst = WorkspaceEventBurst::default();
        let mut event = notify::Event::new(notify::EventKind::Other);
        event.attrs.set_flag(notify::event::Flag::Rescan);
        burst.push(root, Ok(event));
        assert!(burst.rescan);
        assert!(!burst.is_empty());
        let mut burst = WorkspaceEventBurst::default();
        burst.push(root, Err(notify::Error::generic("lost event")));
        assert!(burst.rescan);
        assert!(burst.watcher_failed);
    }

    #[test]
    fn git_object_downloads_and_lock_files_do_not_refresh_the_working_tree() {
        let root = std::path::Path::new("workspace");
        let mut burst = WorkspaceEventBurst::default();
        for relative in [".git/objects/ab/1234", ".git/logs/HEAD", ".git/index.lock", ".git/refs/heads/main.lock"] {
            burst.push(root, Ok(notify::Event::new(notify::EventKind::Any).add_path(root.join(relative))));
        }
        assert!(burst.is_empty());
        for relative in [".git/index", ".git/HEAD", ".git/refs/heads/main", ".git/packed-refs", ".git/info/exclude"] {
            assert!(is_git_metadata_event(root, &root.join(relative)));
        }
    }

    #[test]
    fn tooltip_line_count_has_a_hard_io_limit_and_honors_cancellation() {
        let token = CancellationToken::new();
        assert_eq!(bounded_hover_line_count(&b"one\ntwo\n"[..], &token), Some(3));
        let mut remaining = std::io::Read::take(std::io::repeat(b'\n'), HOVER_LINE_COUNT_MAX_BYTES + 100);
        assert_eq!(bounded_hover_line_count(&mut remaining, &token), None);
        assert_eq!(remaining.limit(), 99, "must stop reading even a growing file");
        token.cancel();
        assert_eq!(bounded_hover_line_count(&mut remaining, &token), None);
        assert_eq!(remaining.limit(), 99);
    }

    #[test]
    fn cpu_worker_pool_always_reserves_interactive_capacity() {
        assert_eq!(background_pool_size(1), 1);
        assert_eq!(background_pool_size(2), 1);
        assert_eq!(background_pool_size(4), 3);
        assert_eq!(background_pool_size(64), 3);
    }

    #[test]
    fn indefinite_ui_wait_wakes_on_exact_notification() {
        let wake = UiWake::default();
        let observed = wake.generation();
        let waiter_wake = wake.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            waiter_wake.wait_for_change(observed, None);
            done_tx.send(()).unwrap();
        });

        ready_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("waiter started");
        wake.notify();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("notification woke indefinite wait");
    }

    #[test]
    fn ui_notification_cannot_overtake_the_transition_into_wait() {
        use std::time::Duration;
        let wake = UiWake::default();
        let observed = wake.generation();
        // Model the UI after it checked the predicate, but before Condvar
        // atomically releases the mutex and parks the thread.
        let guard = wake.wait.0.lock().unwrap();
        let notifier = wake.clone();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            notifier.notify();
            done_tx.send(()).unwrap();
        });
        attempt_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let notified_before_parking = done_rx.recv_timeout(Duration::from_millis(100)).is_ok();
        // Always release the guard and join, including on the regression path.
        let (guard, timeout) = wake.wait.1.wait_timeout_while(
            guard, Duration::from_secs(2), |_| wake.generation() == observed,
        ).unwrap();
        drop(guard);
        producer.join().unwrap();
        assert!(!notified_before_parking, "producer bypassed the wait mutex; its notification could be lost");
        assert!(!timeout.timed_out(), "UI must wake without a second event or timer");
        assert_ne!(wake.generation(), observed);
    }

    #[test]
    fn ui_wait_remembers_work_published_after_a_source_was_polled() {
        use std::time::Duration;
        let wake = UiWake::default();
        let (work_tx, work_rx) = mpsc::channel();
        // This ticket must come from the beginning of the UI turn. The terminal
        // and LSP queues have already been polled when the loop reaches sleep.
        let observed_at_turn_start = wake.generation();
        assert!(work_rx.try_recv().is_err());
        work_tx.send("late PTY/LSP work").unwrap();
        wake.notify();

        let waiter = wake.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            // No timer and no further producer notification: the earlier
            // ticket alone must prevent parking over the queued work.
            waiter.wait_for_change(observed_at_turn_start, None);
            done_tx.send(()).unwrap();
        });
        let finished = done_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        if !finished {
            wake.notify(); // release the test thread before reporting failure
        }
        thread.join().unwrap();
        assert!(finished, "pending work was forgotten until another notification");
        assert_eq!(work_rx.try_recv().unwrap(), "late PTY/LSP work");
    }

    #[test]
    #[ignore = "manual release benchmark of controlled wakeup races"]
    fn benchmark_ui_wakeup_races() {
        use std::time::{Duration, Instant};
        let timer = Duration::from_millis(500); // ordinary cursor-blink deadline
        let mut late_ticket = Vec::new();
        let mut early_ticket = Vec::new();
        let mut lost_notification = Vec::new();
        let mut serialized_notification = Vec::new();
        for _ in 0..5 {
            // The original event-loop order: work arrives after its queue was
            // polled, then a new generation snapshot hides that notification.
            let wake = UiWake::default();
            let before_poll = wake.generation();
            wake.notify();
            let after_poll = wake.generation();
            let started = Instant::now();
            wake.wait_for_change(after_poll, Some(timer));
            late_ticket.push(started.elapsed());
            let started = Instant::now();
            wake.wait_for_change(before_poll, Some(timer));
            early_ticket.push(started.elapsed());

            // Force the old notifier into the exact check/park gap, rather
            // than hoping for a scheduler race in a probabilistic stress test.
            let wake = UiWake::default();
            let guard = wake.wait.0.lock().unwrap();
            let observed = wake.generation();
            assert_eq!(observed, 0); // the waiter just evaluated its predicate
            wake.generation.fetch_add(1, Ordering::Release);
            wake.wait.1.notify_all(); // old notify(), which did not lock
            let started = Instant::now();
            let (guard, timed_out) = wake.wait.1.wait_timeout(guard, timer).unwrap();
            lost_notification.push(started.elapsed());
            assert!(timed_out.timed_out());
            drop(guard);

            let wake = UiWake::default();
            let guard = wake.wait.0.lock().unwrap();
            let observed = wake.generation();
            let notifier = wake.clone();
            let (attempt_tx, attempt_rx) = mpsc::channel();
            let producer = std::thread::spawn(move || {
                attempt_tx.send(()).unwrap();
                notifier.notify();
            });
            attempt_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let started = Instant::now();
            let (guard, timed_out) = wake.wait.1.wait_timeout_while(
                guard, timer, |_| wake.generation() == observed,
            ).unwrap();
            serialized_notification.push(started.elapsed());
            drop(guard);
            producer.join().unwrap();
            assert!(!timed_out.timed_out());
        }
        for samples in [&mut late_ticket, &mut early_ticket, &mut lost_notification, &mut serialized_notification] {
            samples.sort();
        }
        eprintln!("late generation snapshot: before={:?}, after={:?}", late_ticket[2], early_ticket[2]);
        eprintln!("notification in check/park gap: before={:?}, after={:?}", lost_notification[2], serialized_notification[2]);
    }

    #[test]
    fn publishing_background_result_advances_ui_wake_generation() {
        let workers = BackgroundWorkers::new();
        let before = workers.wake_handle().generation();
        workers
            .sender
            .send(BackgroundResult::GitBranch("main".to_string()))
            .expect("result receiver");
        assert!(workers.wake_handle().generation() > before);
    }

    #[test]
    fn result_bursts_are_yielded_in_bounded_batches() {
        let mut workers = BackgroundWorkers::new();
        for index in 0..(MAX_BACKGROUND_RESULTS_PER_POLL + 5) {
            workers
                .sender
                .send(BackgroundResult::GitBranch(index.to_string()))
                .expect("background result receiver");
        }

        assert_eq!(workers.poll().len(), MAX_BACKGROUND_RESULTS_PER_POLL);
        assert!(workers.has_pending_results());
        assert_eq!(workers.poll().len(), 5);
        assert!(!workers.has_pending_results());
    }

    #[test]
    fn result_application_yields_on_time_without_losing_order_or_readiness() {
        let mut workers = BackgroundWorkers::new();
        for index in 0..40 {
            workers.sender.send(BackgroundResult::GitBranch(index.to_string())).unwrap();
        }
        let now = std::time::Instant::now();
        let mut budget = ResultBudget::default();
        for index in 0..2 {
            let BackgroundResult::GitBranch(value) = budget.next_at(&mut workers, now).unwrap() else { panic!() };
            assert_eq!(value, index.to_string());
        }
        assert!(budget.next_at(&mut workers, now + BACKGROUND_RESULT_BUDGET).is_none());
        assert!(workers.has_pending_results());
        // An expensive first result must still leave input a turn, then the
        // next batch resumes at the exact next message, without a fresh wake.
        let mut budget = ResultBudget::default();
        let BackgroundResult::GitBranch(value) = budget.next_at(&mut workers, now).unwrap() else { panic!() };
        assert_eq!(value, "2");
        assert!(budget.next_at(&mut workers, now + std::time::Duration::from_secs(1)).is_none());
        let mut budget = ResultBudget::default();
        for index in 3..35 {
            let BackgroundResult::GitBranch(value) = budget.next_at(&mut workers, now).unwrap() else { panic!() };
            assert_eq!(value, index.to_string());
        }
        assert!(budget.next_at(&mut workers, now).is_none(), "count cap still applies to cheap results");
        let remaining = workers.poll();
        assert_eq!(remaining.len(), 5);
        assert!(!workers.has_pending_results());
        assert_eq!(workers.poll().capacity(), 0, "no allocation for an empty queue");
    }

    #[test]
    fn result_application_uses_the_remaining_shared_budget() {
        let mut workers = BackgroundWorkers::new();
        for index in 0..4 {
            workers.sender.send(BackgroundResult::GitBranch(index.to_string())).unwrap();
        }
        let now = std::time::Instant::now();
        let deadline = now + std::time::Duration::from_millis(1);
        let mut budget = ResultBudget::until(deadline);
        assert!(budget.next_at(&mut workers, now).is_some());
        assert!(budget.next_at(&mut workers, deadline).is_none());
        assert!(workers.has_pending_results());
        // Descheduling before the first message still permits one result,
        // rather than endlessly waking without finishing anything.
        let mut budget = ResultBudget::until(now);
        let BackgroundResult::GitBranch(next) = budget.next_at(&mut workers, deadline).unwrap() else { panic!() };
        assert_eq!(next, "1");
        assert!(budget.next_at(&mut workers, deadline).is_none());
        assert_eq!(workers.poll().len(), 2);
        assert!(!workers.has_pending_results());
    }

    #[test]
    #[ignore = "empty foreground polling allocation benchmark"]
    fn benchmark_empty_result_poll() {
        use std::{hint::black_box, time::Instant};
        let mut workers = BackgroundWorkers::new();
        let mut times = [Vec::new(), Vec::new()];
        for sample in 0..9 {
            for reference in [sample % 2 == 0, sample % 2 != 0] {
                let started = Instant::now();
                for _ in 0..100_000 {
                    if reference {
                        // The old foreground poll reserved a full result batch
                        // before finding out whether there were any messages.
                        let mut results = Vec::<BackgroundResult>::with_capacity(MAX_BACKGROUND_RESULTS_PER_POLL);
                        if let Ok(result) = workers.receiver.try_recv() { results.push(result); }
                        black_box(results);
                    } else {
                        black_box(ResultBudget::default().next(black_box(&mut workers)));
                    }
                }
                times[usize::from(!reference)].push(started.elapsed() / 100_000);
            }
        }
        for samples in &mut times { samples.sort_unstable(); }
        eprintln!("Empty background poll: {:?} -> {:?}; removes {}-byte reservation per poll",
            times[0][4], times[1][4], MAX_BACKGROUND_RESULTS_PER_POLL * std::mem::size_of::<BackgroundResult>());
    }

    #[test]
    fn saves_have_a_bounded_reusable_pool_independent_of_cpu_work() {
        for cpus in [1, 2, 3, 8, 256] {
            assert_eq!(save_pool_size(cpus), if cpus <= 2 { 1 } else { 2 });
        }
        let mut workers = BackgroundWorkers::new();
        let (started, running) = mpsc::channel();
        let (release, receiver) = mpsc::channel();
        let receiver = Arc::new(std::sync::Mutex::new(receiver));
        let initial_slots = workers.queue_snapshot().2;
        assert!(workers.save_pool_sender.is_none());
        for _ in 0..20 {
            let started = started.clone();
            let receiver = receiver.clone();
            workers.save_for_file(std::path::Path::new("file.txt"), move || {
                started.send(()).unwrap();
                receiver.lock().unwrap().recv().unwrap();
            });
        }
        let slots = workers.save_pool_sender.as_ref().unwrap().load.live.load(Ordering::Relaxed);
        assert!((1..=2).contains(&slots));
        for _ in 0..slots { running.recv_timeout(std::time::Duration::from_secs(5)).unwrap(); }
        assert_eq!(workers.active_job_count(), 20);
        assert_eq!(workers.queue_snapshot(), (20 - slots, slots, initial_slots + slots, 0));
        assert!(workers.join_handles.is_empty(), "saving does not spawn a thread per document");
        // A blocked filesystem must not occupy syntax/diff worker slots.
        let (cpu_done, cpu_wait) = mpsc::channel();
        workers.pool_sender().send(Box::new(move || cpu_done.send(()).unwrap())).unwrap();
        cpu_wait.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        for _ in 0..20 { release.send(()).unwrap(); }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while workers.has_active_jobs() && std::time::Instant::now() < deadline { std::thread::yield_now(); }
        assert!(!workers.has_active_jobs());
        assert_eq!(workers.queue_snapshot(), (0, 0, initial_slots + slots, 0));
        let (done, wait) = mpsc::channel();
        workers.save_for_file(std::path::Path::new("next.txt"), move || done.send(()).unwrap());
        wait.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(workers.queue_snapshot().2, initial_slots + slots);
    }

    #[test]
    fn pool_activity_is_exact_while_a_job_is_blocked() {
        let workers = BackgroundWorkers::new();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        workers
            .pool_sender()
            .send(Box::new(move || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }))
            .unwrap();

        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert!(workers.has_active_jobs());
        let (queued, running, slots, results) = workers.queue_snapshot();
        assert_eq!((queued, running, results), (0, 1, 0));
        assert!((1..=3).contains(&slots));
        release_tx.send(()).unwrap();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while workers.has_active_jobs() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!workers.has_active_jobs());
        assert_eq!(workers.queue_snapshot().1, 0);
    }

    #[test]
    fn pool_queue_counters_survive_failure_and_panics_without_os_sampling() {
        let (sender, receiver) = queue::channel();
        let load = Arc::new(PoolLoad::default());
        let active_jobs = Arc::new(AtomicUsize::new(0));
        let pool = BackgroundTaskSender { sender, priority: queue::Priority::Analysis, active_jobs: active_jobs.clone(), load: load.clone(), wake: UiWake::default() };
        let worker_load = load.clone();
        pool.send(Box::new(move || {
            assert_eq!(worker_load.queued.load(Ordering::Relaxed), 0);
            assert_eq!(worker_load.running.load(Ordering::Relaxed), 1);
            panic!("test worker unwinding");
        })).unwrap();
        assert_eq!(load.queued.load(Ordering::Relaxed), 1);
        assert_eq!(load.running.load(Ordering::Relaxed), 0);
        let task = receiver.recv().unwrap();
        load.live.store(1, Ordering::Relaxed);
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _thread_guard = PoolThreadGuard(load.clone());
            task.run();
        })).is_err());
        assert_eq!(load.live.load(Ordering::Relaxed), 0);
        assert_eq!(load.queued.load(Ordering::Relaxed), 0);
        assert_eq!(load.running.load(Ordering::Relaxed), 0);
        assert_eq!(active_jobs.load(Ordering::Relaxed), 0);
        pool.send(Box::new(|| panic!("abandoned queued work must not run"))).unwrap();
        assert_eq!(load.queued.load(Ordering::Relaxed), 1);
        drop(receiver);
        assert_eq!(load.queued.load(Ordering::Relaxed), 0);
        assert_eq!(active_jobs.load(Ordering::Relaxed), 0);
        assert!(pool.send(Box::new(|| panic!("disconnected task must not run"))).is_err());
        assert_eq!(load.queued.load(Ordering::Relaxed), 0);
        assert_eq!(active_jobs.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cancellation_token_starts_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancellation_token_cancel_sets_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancellation_token_clone_shares_state() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!clone.is_cancelled());
        token.cancel();
        assert!(clone.is_cancelled());
    }

    #[test]
    fn cancellation_token_multiple_cancels_idempotent() {
        let token = CancellationToken::new();
        token.cancel();
        token.cancel();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn replacing_workspace_watcher_cancels_previous_watcher() {
        let mut workers = BackgroundWorkers::new();
        workers.start_file_watcher(PathBuf::from("missing-workspace-one"));
        let first = workers
            .file_watcher_control
            .as_ref()
            .expect("first watcher control")
            .clone();

        workers.start_file_watcher(PathBuf::from("missing-workspace-two"));

        assert!(first.is_stopped());
        assert!(workers.file_watcher_control.is_some());
    }

    #[test]
    fn workspace_events_keep_all_project_directory_names() {
        let root = PathBuf::from("workspace");
        for path in [
            root.join("target").join("debug").join("vtcode.exe"),
            root.join("extensions")
                .join("x")
                .join("node_modules")
                .join("package.js"),
            root.join("src").join("__pycache__").join("module.pyc"),
            root.join("build").join("source.rs"),
            root.join("dist").join("source.js"),
            root.join(".venv").join("source.py"),
            root.join("venv").join("source.py"),
            root.join(".tox").join("source.py"),
            root.join(".mypy_cache").join("source.py"),
            root.join(".pytest_cache").join("source.py"),
        ] {
            assert!(!is_ignored_workspace_event(&root, &path), "{}", path.display());
            let mut burst = WorkspaceEventBurst::default();
            burst.push(&root, Ok(notify::Event::new(notify::EventKind::Modify(
                notify::event::ModifyKind::Data(notify::event::DataChange::Any))).add_path(path.clone())));
            assert_eq!(coalesce_workspace_event_paths(&root, burst.paths), [path]);
        }
        assert!(is_ignored_workspace_event(&root, &root.join(".git/index.lock")));
        assert!(!is_ignored_workspace_event(
            &root,
            &root.join("src").join("main.rs")
        ));
    }

    #[test]
    fn formatter_working_copies_do_not_trigger_workspace_refreshes() {
        use notify::event::{CreateKind, ModifyKind, RemoveKind};
        use notify::EventKind;
        let root = PathBuf::from("workspace");
        let temporary = root.join("src").join(".vtcode-format-test");
        let file = temporary.join("main.py");
        let mut burst = WorkspaceEventBurst::default();
        for (kind, path) in [
            (EventKind::Create(CreateKind::Folder), temporary.clone()),
            (EventKind::Create(CreateKind::File), file.clone()),
            (EventKind::Modify(ModifyKind::Any), file.clone()),
            (EventKind::Remove(RemoveKind::File), file.clone()),
            (EventKind::Remove(RemoveKind::Folder), temporary.clone()),
        ] {
            burst.push(&root, Ok(notify::Event::new(kind).add_path(path)));
        }
        assert!(burst.is_empty());
        assert!(!burst.structure_changed);
        assert!(coalesce_workspace_event_paths(&root, vec![temporary, file]).is_empty());
    }

    #[test]
    fn git_metadata_events_are_detected_without_becoming_file_changes() {
        let root = PathBuf::from("workspace");
        let head = root.join(".git").join("HEAD");
        assert!(is_git_metadata_event(&root, &head));
        assert!(coalesce_workspace_event_paths(&root, vec![head]).is_empty());
    }

    #[test]
    fn nested_repository_metadata_refreshes_git_but_objects_and_locks_stay_quiet() {
        let root = PathBuf::from("workspace");
        for suffix in ["index", "HEAD", "refs/heads/main", "info/exclude"] {
            let path = root.join("src/project/.git").join(suffix);
            assert!(is_git_metadata_event(&root, &path));
            let mut burst = WorkspaceEventBurst::default();
            burst.push(&root, Ok(notify::Event::new(notify::EventKind::Any).add_path(path)));
            assert!(burst.git_metadata_changed);
            assert!(burst.paths.is_empty());
        }
        for suffix in ["objects/ab/1234", "logs/HEAD", "index.lock", "refs/heads/main.lock"] {
            let mut burst = WorkspaceEventBurst::default();
            burst.push(&root, Ok(notify::Event::new(notify::EventKind::Any).add_path(root.join("src/project/.git").join(suffix))));
            assert!(burst.is_empty());
        }
        for kind in [notify::EventKind::Create(notify::event::CreateKind::Folder),
            notify::EventKind::Remove(notify::event::RemoveKind::File)]
        {
            let marker = root.join("src/project/.git");
            let mut burst = WorkspaceEventBurst::default();
            burst.push(&root, Ok(notify::Event::new(kind).add_path(marker.clone())));
            assert!(burst.git_metadata_changed && burst.structure_changed);
            assert_eq!(burst.paths, vec![marker]);
        }
    }

    #[test]
    fn native_watcher_coverage_keeps_project_directories_and_excludes_external_paths() {
        let root = PathBuf::from("workspace");
        assert!(workspace_path_is_watched(&root, &root.join("src").join("main.rs")));
        assert!(workspace_path_is_watched(
            &root,
            &root.join("target").join("debug").join("vtcode.exe")
        ));
        assert!(!workspace_path_is_watched(
            &root,
            &PathBuf::from("elsewhere").join("file.rs")
        ));
    }

    #[test]
    fn workspace_event_burst_is_filtered_sorted_and_deduplicated() {
        let root = PathBuf::from("workspace");
        let main = root.join("src").join("main.rs");
        let lib = root.join("src").join("lib.rs");
        let paths = coalesce_workspace_event_paths(
            &root,
            vec![
                main.clone(),
                root.join("target").join("debug").join("artifact"),
                lib.clone(),
                main.clone(),
            ],
        );
        assert_eq!(paths, vec![lib, main, root.join("target/debug/artifact")]);
    }
}
