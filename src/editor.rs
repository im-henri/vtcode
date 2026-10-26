use std::borrow::Cow;
use std::collections::VecDeque;
use std::fs;
use std::io;
#[cfg(test)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use ropey::Rope;

#[path = "editor_save.rs"]
pub(crate) mod save;
pub(crate) use save::DiskSaveBaseline;
pub(crate) use save::prepare_rope as prepare_save_rope;

static SAVE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
/// Large sequential reads benefit substantially from a buffer wider than
/// `BufReader`'s conservative default, especially on networked workspaces.
const FILE_READ_BUFFER_BYTES: usize = 256 * 1024;

/// Text decoded from disk without doing any editor/UI work. Large-file opens
/// use this on a background thread, so the terminal event loop remains free to
/// draw and handle input while disk I/O and rope construction are in progress.
#[derive(Clone)]
pub(crate) struct LoadedTextFile {
    pub remote_checkpoint: Option<Arc<crate::remote::Checkpoint>>,
    pub rope: Rope,
    pub encoding: &'static str,
    pub file_size: u64,
    pub last_disk_modified: Option<std::time::SystemTime>,
}

/// Dense gutter markers plus their sparse overview representation, prepared
/// away from the terminal thread. `Arc` keeps split views O(1) to synchronize.
#[derive(Clone, Debug)]
pub(crate) struct PreparedGitLineStatus {
    markers: Arc<[char]>,
    changes: Arc<[(usize, char)]>,
    signature: u64,
}

impl PreparedGitLineStatus {
    pub(crate) fn from_diffs(line_count: usize, staged: &[char], unstaged: &[char]) -> Self {
        let mut markers = Vec::with_capacity(line_count);
        let mut changes = Vec::new();
        let mut signature = 0xcbf29ce484222325u64;
        for line in 0..line_count {
            let unstaged = unstaged.get(line).copied().unwrap_or(' ');
            let staged = staged.get(line).copied().unwrap_or(' ');
            let marker = if unstaged == 'D' || (unstaged == ' ' && staged == 'D') {
                'D'
            } else if unstaged != ' ' {
                'U'
            } else if staged != ' ' {
                'S'
            } else {
                ' '
            };
            if marker != ' ' {
                changes.push((line, marker));
            }
            signature = (signature ^ u64::from(marker as u32)).wrapping_mul(0x100000001b3);
            markers.push(marker);
        }
        signature ^= line_count as u64;
        Self {
            markers: markers.into(),
            changes: changes.into(),
            signature,
        }
    }

    pub(crate) fn from_markers(markers: Vec<char>) -> Self {
        let changes = markers
            .iter()
            .enumerate()
            .filter_map(|(line, &marker)| (marker != ' ').then_some((line, marker)))
            .collect::<Vec<_>>();
        let mut signature = 0xcbf29ce484222325u64;
        for &marker in &markers {
            signature = (signature ^ u64::from(marker as u32)).wrapping_mul(0x100000001b3);
        }
        signature ^= markers.len() as u64;
        Self {
            markers: markers.into(),
            changes: changes.into(),
            signature,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.markers.len()
    }

    #[cfg(test)]
    pub(crate) fn markers(&self) -> &[char] {
        &self.markers
    }
}

/// Immutable data needed to save an editor snapshot. Constructing this only
/// clones the Rope tree; transformations, encoding, and disk I/O happen later
/// on a worker for files of every size.
#[derive(Clone)]
pub(crate) struct BackgroundSaveRequest {
    pub source_document_id: u64,
    pub path: PathBuf,
    pub rope: Rope,
    pub encoding: &'static str,
    pub line_ending: &'static str,
    pub trim_trailing: bool,
    pub insert_final_newline: bool,
    pub source_content_version: u64,
    pub fail_if_exists: bool,
    pub expected_disk: Option<DiskSaveBaseline>,
    /// Options already applied to expected_disk.rope by a successful save.
    pub prepared_options: Option<SaveOptions>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SaveOptions {
    pub trim_trailing: bool,
    pub insert_final_newline: bool,
    pub line_ending: &'static str,
}
impl BackgroundSaveRequest {
    pub(crate) fn prepare_text(&self) -> Rope {
        if self.prepared_options == Some(self.options())
            && self.expected_disk.as_ref().is_some_and(|saved| self.rope.is_instance(&saved.rope))
        {
            return self.rope.clone();
        }
        prepare_save_rope(&self.rope, self.trim_trailing, self.insert_final_newline, self.line_ending == "CRLF")
    }

    pub(crate) fn options(&self) -> SaveOptions {
        SaveOptions { trim_trailing: self.trim_trailing, insert_final_newline: self.insert_final_newline, line_ending: self.line_ending }
    }
    pub(crate) fn same_intent(&self, other: &Self) -> bool {
        self.path == other.path && self.source_document_id == other.source_document_id
            && self.source_content_version == other.source_content_version
            && self.rope.is_instance(&other.rope) && self.encoding == other.encoding
            && self.options() == other.options() && self.fail_if_exists == other.fail_if_exists
            && match (&self.expected_disk, &other.expected_disk) {
                (None, None) => true,
                (Some(a), Some(b)) => a.same_snapshot(b),
                _ => false,
            }
    }
}

/// Keep the actual pending write, shared with its worker. Versions alone cannot
/// distinguish save options, a reopened document or another encoding. Matching
/// uses Rope identity, never a whole-file comparison on the input thread.
pub(crate) struct SaveIntent {
    pub version: u64,
    request: Option<Arc<BackgroundSaveRequest>>,
}
impl SaveIntent {
    pub(crate) fn new(request: Arc<BackgroundSaveRequest>) -> Self {
        Self { version: request.source_content_version, request: Some(request) }
    }
    pub(crate) fn matches(&self, request: &BackgroundSaveRequest) -> bool {
        self.request.as_ref().is_some_and(|previous| previous.same_intent(request))
    }
    pub(crate) fn completed_options(&self, document: u64, version: u64, rope: &Rope) -> Option<SaveOptions> {
        self.request.as_ref().filter(|request| request.source_document_id == document
            && request.source_content_version == version && request.rope.is_instance(rope)).map(|request| request.options())
    }
}
#[cfg(test)]
impl From<u64> for SaveIntent {
    fn from(version: u64) -> Self {
        // Existing fixtures can mark unknown pending work. It must never be
        // mistaken for a provably identical real save request.
        Self { version, request: None }
    }
}

/// Prepare and write immutable text without creating an editor or undo history.
pub(crate) use save::write as write_background_save;

/// Read a text file into a rope with a single full-file pass for the common
/// UTF-8 case. The old background loader first retained `fs::read()` for the
/// entire file and then streamed the same file into a rope, doubling both disk
/// traffic and peak resident memory for large files.
pub(crate) fn load_text_file(path: &Path) -> io::Result<LoadedTextFile> {
    let (file, metadata) = crate::fs_ops::open_document_source(path, false)?;
    decode_text_source(file, metadata, path, None)
}

pub(crate) fn decode_text_source(file: fs::File, metadata: fs::Metadata, path: &Path, cancel: Option<&crate::background::CancellationToken>) -> io::Result<LoadedTextFile> {
    let file_size = metadata.len();
    let last_disk_modified = metadata.modified().ok();
    let result = if let Some(cancel) = cancel {
        load::read_text_stream_cancellable(file, path, file_size, cancel)
    } else { read_text_stream(file, path, file_size) };
    if cancel.is_some_and(|cancel| cancel.is_cancelled()) { return Err(io::ErrorKind::Interrupted.into()); }
    let (rope, encoding) = result?;
    Ok(LoadedTextFile { rope, encoding, file_size, last_disk_modified, remote_checkpoint: None })
}

pub(crate) fn rope_line_ending(rope: &Rope) -> &'static str {
    // Only inspect line tails. Advancing a line iterator through a multi-
    // chunk log record can visit its leaves even though the Rope already
    // indexes its end. This also runs when importing saved undo snapshots.
    for index in 0..rope.len_lines().min(10) {
        let line = rope.line(index);
        let len = line.len_chars();
        if len > 0 && line.char(len - 1) == '\n' {
            return if len > 1 && line.char(len - 2) == '\r' { "CRLF" } else { "LF" };
        }
    }
    "LF"
}

#[path = "editor_load.rs"]
mod load;
use load::read_text_stream;

/// Return a unique temporary path beside the target file. Keeping the original
/// extension in the name prevents `foo.rs` and `foo.txt` saves from colliding.
pub(crate) fn save_temp_path(path: &Path) -> PathBuf {
    let id = SAVE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let suffix = format!(".vtcode-tmp-{}-{id}", std::process::id());
    let mut file_name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_else(|| std::ffi::OsString::from("untitled"));
    file_name.push(suffix);
    path.with_file_name(file_name)
}

pub(crate) fn is_save_temporary_path(path: &Path) -> bool {
    let Some(name) = path.file_name() else { return false; };
    let bytes = name.as_encoded_bytes();
    let marker = b".vtcode-tmp-";
    let Some(start) = bytes.windows(marker.len()).rposition(|part| part == marker) else { return false; };
    let suffix = &bytes[start + marker.len()..];
    let Some(dash) = suffix.iter().position(|byte| *byte == b'-') else { return false; };
    dash != 0 && dash + 1 < suffix.len()
        && suffix[..dash].iter().all(u8::is_ascii_digit)
        && suffix[dash + 1..].iter().all(u8::is_ascii_digit)
}

#[path = "editor_unsaved_diff.rs"]
mod unsaved_diff;
pub(crate) use unsaved_diff::{UnsavedDiff, compute_unsaved_diff_markers_cancellable};

/// Compute exact per-line unsaved-change markers without mutating an editor.
/// Rope clones are cheap, so callers can run this on a background worker while
/// the UI continues editing the live buffer.
#[cfg(test)]
pub(crate) fn compute_unsaved_diff_markers(saved: &Rope, current: &Rope) -> UnsavedDiff {
    compute_unsaved_diff_markers_cancellable(saved, current, None, &|| false)
        .expect("uncancelled diff")
}

#[cfg(test)]
#[path = "editor_unsaved_diff_tests.rs"]
mod unsaved_diff_tests;

/// Type alias for selection snapshot: optional (anchor, cursor) pair.
pub(crate) type SelectionSnapshot = (Option<(usize, usize)>, (usize, usize));

/// Type alias for persisted undo/redo snapshots. The final field records the
/// exact modified state, avoiding a whole-document comparison during restore.
pub(crate) type UndoSnapshot = (
    Rope,
    usize,
    usize,
    Vec<(usize, usize)>,
    Vec<Option<(usize, usize)>>,
    Option<(usize, usize)>,
    bool,
);

/// Ordered by source line so local edits can invalidate a range without
/// visiting every row cached while browsing the document.
type WrapCache = std::collections::BTreeMap<(usize, usize, usize), std::rc::Rc<Vec<(usize, usize)>>>;

#[path = "editor_rope_search.rs"]
mod occurrence_search;
pub(crate) use occurrence_search::RopeSearch;

/// Return the leading whitespace of a line (excluding newline characters).
#[cfg(test)]
pub(crate) fn leading_whitespace(line: &str) -> &str {
    let end = line
        .find(|c: char| !c.is_whitespace() || c == '\n' || c == '\r')
        .unwrap_or(line.len());
    &line[..end]
}

/// Count the number of leading whitespace characters on a line (excluding newline).
pub(crate) fn leading_whitespace_count(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace() && *c != '\n' && *c != '\r')
        .count()
}
pub(crate) fn sort_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            for key in keys {
                sorted.insert(key.clone(), sort_json_value(&map[key]));
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sort_json_value).collect())
        }
        other => other.clone(),
    }
}

/// Split text into words by common separators (spaces, underscores, hyphens, camelCase boundaries)
pub(crate) fn split_into_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut prev: Option<char> = None;
    for c in text.chars() {
        if c == '_' || c == '-' || c == ' ' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else if c.is_uppercase() && prev.is_some_and(char::is_lowercase) {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.push(c);
        } else {
            current.push(c);
        }
        prev = Some(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Extract an HTML/XML tag name from text ending with '>'
/// e.g. "<div>" → Some("div"), "<div class='x'>" → Some("div")
/// Self-closing tags like "<br/>" or void elements return None
pub(crate) fn is_markup_language(lang: &str) -> bool {
    matches!(
        lang,
        "HTML" | "XML" | "SVG" | "Vue" | "Svelte" | "JavaScript React" | "TypeScript React"
    )
}

pub(crate) fn extract_opening_tag(text: &str) -> Option<&str> {
    let text = text.trim_end_matches('>');
    let lt_pos = text.rfind('<')?;
    let inside = &text[lt_pos + 1..];
    if inside.starts_with('/') || inside.ends_with('/') {
        return None; // Closing tag or self-closing
    }
    // Skip void HTML elements
    let tag_name = inside.split(|c: char| c.is_whitespace()).next()?;
    if tag_name.is_empty() || tag_name.starts_with('!') || tag_name.starts_with('?') {
        return None;
    }
    let void_elements = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];
    if void_elements
        .iter()
        .any(|e| tag_name.eq_ignore_ascii_case(e))
    {
        return None;
    }
    Some(tag_name)
}

#[derive(Clone)]
pub(crate) struct UndoState {
    /// Identifies this snapshot across views of the same document. Mutable
    /// dirty-range/checkpoint metadata does not change its identity.
    pub snapshot_id: u64,
    pub rope: Rope,
    pub line_ending: &'static str,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub extra_cursors: Vec<(usize, usize)>,
    pub extra_cursor_anchors: Vec<Option<(usize, usize)>>,
    pub selection_anchor: Option<(usize, usize)>,
    /// Exact dirty state at this snapshot, making Undo/Redo independent of
    /// document size.
    pub modified: bool,
    /// Bounded line range affected by the edit represented by this snapshot.
    /// Undo/redo uses it to retain syntax cache entries above and below the
    /// edit instead of invalidating the whole document.
    pub dirty_from: usize,
    pub dirty_through: usize,
    pub change_id: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum CharClass {
    None,
    Whitespace,
    AlphaNumeric,
    Punctuation,
    Newline,
}

impl CharClass {
    pub(crate) fn of(c: char) -> Self {
        if c == '\n' || c == '\r' {
            Self::Newline
        } else if c.is_whitespace() {
            Self::Whitespace
        } else if c.is_alphanumeric() || c == '_' {
            Self::AlphaNumeric
        } else {
            Self::Punctuation
        }
    }
}

/// Default threshold above which expensive language and whole-file analysis is paused.
pub(crate) const DEFAULT_LARGE_FILE_THRESHOLD_MB: u64 = 50;
pub(crate) const LARGE_FILE_THRESHOLD: u64 = DEFAULT_LARGE_FILE_THRESHOLD_MB * 1024 * 1024;
pub(crate) const DEFAULT_LARGE_FILE_ANALYSIS_TIMEOUT_MS: u64 = 2_000;
/// Historical threshold retained by loading regression tests. Production now
/// sends every file through the non-blocking loader.
#[cfg(test)]
pub(crate) const BG_LOAD_THRESHOLD: u64 = 1024 * 1024;

pub(crate) struct Editor {
    pub(crate) document: crate::editor_document::DocumentView,
    /// Stable view identity, independent of tab order, file name and split.
    pub(crate) navigation_id: u64,
    /// Shared by duplicate views; a new Save As destination starts a new identity.
    pub(crate) save_identity: u64,
    pub rope: Rope,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub scroll_offset: usize,
    pub(crate) markdown_preview_scroll: Option<crate::ui_markdown::PreviewScroll>,
    pub wrap_scroll_offset: usize,
    pub horizontal_scroll: usize,
    pub file_path: Option<PathBuf>,
    /// A display/save suggestion for generated buffers, never a filesystem path.
    pub suggested_file_name: Option<String>,
    pub modified: bool,
    pub read_only: bool,
    pub overwrite_mode: bool,
    pub viewport_height: usize,
    pub viewport_width: usize,
    pub(crate) ai_review: crate::editor_ai_review::ReviewView,
    /// Visible source rows in a compact diff; the complete rope stays intact.
    pub(crate) diff_display_rows: Option<Vec<Option<usize>>>,
    pub(crate) diff_text_inset: usize,
    pub word_wrap: bool,
    /// Word wrap column (0 = viewport width, >0 = fixed column)
    pub word_wrap_column: usize,
    pub whitespace_mode: crate::app::WhitespaceMode,
    pub tab_size: usize,
    pub use_tabs: bool,
    // Selection: anchor is where selection started, cursor is where it extends to
    pub selection_anchor: Option<(usize, usize)>, // (line, col)
    // Clipboard
    pub clipboard: String,
    pub clipboard_is_line: bool,
    pub(crate) clipboard_failed: bool,
    // Undo/Redo
    pub(crate) undo_stack: crate::editor_document::HistoryStack,
    pub(crate) redo_stack: crate::editor_document::HistoryStack,
    /// Distinguishes history navigation from ordinary edits for visual feedback.
    pub(crate) animation_undo_revision: u64,
    // Track changes for grouping undo snapshots
    pub(crate) undo_group_active: bool,
    pub(crate) last_undo_time: std::time::Instant,
    pub(crate) last_undo_char_class: CharClass,
    pub(crate) next_undo_change_id: u64,
    pub(crate) active_undo_change_id: Option<u64>,
    // Original rope content (for detecting when undo returns to saved state, and for diff gutter)
    pub saved_rope: Option<Rope>,
    /// Encoding of the saved snapshot, independent of pending encoding changes.
    pub(crate) saved_encoding: &'static str,
    pub(crate) saved_save_options: Option<SaveOptions>,
    pub(crate) remote_checkpoint: Option<Arc<crate::remote::Checkpoint>>,
    // Code folding: maps fold start line -> fold end line (exclusive)
    pub folded_ranges: std::collections::BTreeMap<usize, usize>,
    pub(crate) fold_buttons: crate::ui_editor_folds::Buttons,
    // Line bookmarks
    pub bookmarks: std::collections::BTreeSet<usize>,
    // Detected encoding
    pub encoding: &'static str,
    pub last_disk_modified: Option<std::time::SystemTime>,
    /// Cached filesystem availability updated by background loads, saves, and
    /// watcher probes. Rendering must never call `Path::exists()` itself.
    pub(crate) disk_unavailable: bool,
    /// The buffer contains an open-error message, not document contents.
    pub(crate) load_failed: bool,
    // Multi-cursor: additional cursor positions (line, col)
    pub extra_cursors: Vec<(usize, usize)>,
    pub extra_cursor_anchors: Vec<Option<(usize, usize)>>,
    pub typewriter_mode: bool,
    pub cursor_surrounding_lines: usize,
    pub language_override: Option<String>,
    /// Custom file extension → language associations from settings
    pub language_associations: rustc_hash::FxHashMap<String, String>,
    pub(crate) scope_bubble_rules: std::sync::Arc<crate::scope_bubble_config::Rules>,
    pub content_version: u64,
    pub(crate) merge_conflicts: crate::merge_conflicts::Cache,
    /// Edits not yet handed to the syntax cache. Scheduling acknowledges this
    /// range; the cache separately tracks pending parser propagation so later
    /// newline edits can shift their own suffix without shifting earlier edits.
    pub(crate) syntax_dirty_from: Option<usize>,
    pub(crate) syntax_dirty_through: Option<usize>,
    /// Cached unsaved-changes diff (computed via `similar` crate).
    /// Tuple: (version_at_compute_time, exact sparse change ranges).
    unsaved_diff_cache: (u64, UnsavedDiff),
    /// Edit range used only to project the previous exact diff while the
    /// deferred diff worker catches up. Syntax can converge much earlier, so
    /// this lifetime must not be tied to `syntax_dirty_*`.
    unsaved_diff_dirty_from: Option<usize>,
    unsaved_diff_dirty_through: Option<usize>,
    /// Sparse Git markers used by overview widgets.
    git_change_lines: Arc<[(usize, char)]>,
    git_line_status_signature: u64,
    /// Invalidates render-side overview caches when background markers arrive.
    pub(crate) overview_change_revision: u64,
    /// "Sticky" screen column within the visual row, preserved through shorter lines.
    pub desired_col: Option<usize>,
    /// Per-line change status:
    /// 'N' = unsaved buffer change, 'U' = unstaged git change, 'S' = staged git change, ' ' = unchanged.
    git_line_status: Arc<[char]>,
    /// Large file mode: disables expensive parsing and whole-document features.
    pub large_file_mode: bool,
    /// A below-threshold file can also enter large-file mode after analysis times out.
    pub rich_features_timed_out: bool,
    /// File size in bytes of the currently opened file
    pub file_size: u64,
    /// LSP folding ranges as (start_line, end_line) pairs, sorted by start_line
    pub lsp_folding_ranges: Vec<(usize, usize)>,
    /// Text-derived gutter hints, reused across cursor and selection redraws.
    pub(crate) fold_hint_cache: std::cell::RefCell<crate::editor_transform::FoldHintCache>,
    /// Bounded decoded viewport text reused across scrolling and redraws.
    pub(crate) render_line_cache: std::cell::RefCell<crate::ui_editor_highlight::RenderLineCache>,
    pub(crate) minimap_cache: std::cell::RefCell<crate::ui_editor_minimap::MinimapCache>,
    /// Cached visible line count (invalidated on fold/unfold/content change)
    pub cached_visible_line_count: Option<usize>,
    /// Cached wrapped total rows (invalidated on content/fold change)
    pub cached_wrapped_total: Option<(usize, usize)>, // (wrap_width, total_rows)
    /// Visual-row prefix tree, supporting local row-count changes and indexed
    /// scrolling without rebuilding offsets for every later line.
    pub(crate) cached_row_offsets: Option<(usize, crate::editor_wrap::WrapRowOffsets)>,
    pub scroll_past_end: bool,
    /// Word wrap range cache: (line_idx, line_hash, wrap_width) -> ranges
    pub wrap_cache: WrapCache,
    pub wrap_cache_version: u64,
    pub(crate) wrap_cache_line_count: usize,
    pub(crate) wrap_dirty_range: Option<(usize, usize)>,
    pub(crate) wrap_revision: u64,
    /// Cursor position history for Ctrl+U (cursor undo)
    pub(crate) cursor_position_stack: VecDeque<(usize, usize)>,
    /// Selection expansion history for shrink_selection
    pub selection_history: VecDeque<SelectionSnapshot>,
    pub(crate) selection_history_source: Option<Rope>,
    /// Cached line ending detection result (computed once on load, not per-frame)
    pub cached_line_ending: &'static str,
    /// Whether this editor is waiting for background file loading to complete
    pub bg_loading: bool,
    /// Explicit navigation requested while loading, applied after session restoration.
    /// Stored on the editor so it follows its tab across editor groups.
    pub(crate) pending_cursor_position: Option<(usize, usize)>,
    pub(crate) pending_cursor_utf16: bool,
    pub(crate) pending_navigation_selection: Option<crate::app_navigation::NavigationSelection>,
    /// Session cursor/undo/unsaved state applied after the asynchronous disk
    /// snapshot arrives. Keeping this on the placeholder supports duplicate
    /// tabs with different cursor positions.
    pub(crate) pending_session_restore: Option<crate::session::SessionRestore>,
    pub(crate) blame: crate::editor_blame::View,
    /// Active snippet tab stops (index, line, col, placeholder_len)
    pub snippet_tab_stops: Vec<SnippetTabStop>,
    /// Current snippet tab stop index (which $N we're on)
    pub snippet_active_stop: usize,
    /// Whether snippet mode is active
    pub snippet_active: bool,
    /// Snapshot used to relocate placeholders lazily when navigating a snippet.
    pub(crate) snippet_rope: Option<Rope>,
    /// Editorconfig: override trim trailing whitespace on save
    pub ec_trim_trailing: Option<bool>,
    /// Editorconfig: override insert final newline on save
    pub ec_insert_final_newline: Option<bool>,
}

/// A snippet tab stop position
#[derive(Debug, Clone)]
pub(crate) struct SnippetTabStop {
    pub index: u32,
    pub line: usize,
    pub col: usize,
    pub placeholder_len: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct WrappedRowInfo {
    pub line_idx: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub line_len: usize,
    pub is_first: bool,
}

impl Editor {
    pub(crate) fn allocate_navigation_id() -> u64 {
        static NEXT_NAVIGATION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT_NAVIGATION_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn new() -> Self {
        let identity = Self::allocate_navigation_id();
        Self {
            navigation_id: identity,
            save_identity: identity,
            document: Default::default(),
            rope: Rope::new(),
            cursor_line: 0,
            cursor_col: 0,
            scroll_offset: 0,
            markdown_preview_scroll: None,
            wrap_scroll_offset: 0,
            horizontal_scroll: 0,
            file_path: None,
            suggested_file_name: None,
            modified: false,
            read_only: false,
            overwrite_mode: false,
            viewport_height: 24,
            viewport_width: 80,
            ai_review: Default::default(),
            diff_display_rows: None,
            diff_text_inset: 0,
            word_wrap: false,
            word_wrap_column: 0,
            whitespace_mode: crate::app::WhitespaceMode::None,
            tab_size: 4,
            use_tabs: false,
            selection_anchor: None,
            clipboard: String::new(),
            clipboard_is_line: false,
            clipboard_failed: false,
            undo_stack: Default::default(),
            redo_stack: Default::default(),
            animation_undo_revision: 0,
            undo_group_active: false,
            last_undo_time: std::time::Instant::now(),
            last_undo_char_class: CharClass::None,
            next_undo_change_id: 0,
            active_undo_change_id: None,
            saved_rope: None,
            saved_encoding: "UTF-8",
            saved_save_options: None,
            remote_checkpoint: None,
            folded_ranges: std::collections::BTreeMap::new(),
            fold_buttons: Default::default(),
            bookmarks: std::collections::BTreeSet::new(),
            encoding: "UTF-8",
            last_disk_modified: None,
            disk_unavailable: false,
            load_failed: false,
            extra_cursors: Vec::new(),
            extra_cursor_anchors: Vec::new(),
            typewriter_mode: false,
            cursor_surrounding_lines: 5,
            language_override: None,
            language_associations: rustc_hash::FxHashMap::default(),
            scope_bubble_rules: crate::scope_bubble_config::Rules::empty(),
            content_version: 0,
            merge_conflicts: Default::default(),
            syntax_dirty_from: None,
            syntax_dirty_through: None,
            unsaved_diff_cache: (u64::MAX, UnsavedDiff::default()),
            unsaved_diff_dirty_from: None,
            unsaved_diff_dirty_through: None,
            git_change_lines: Arc::from([]),
            git_line_status_signature: 0,
            overview_change_revision: 0,
            desired_col: None,
            git_line_status: Arc::from([]),
            large_file_mode: false,
            rich_features_timed_out: false,
            file_size: 0,
            lsp_folding_ranges: Vec::new(),
            fold_hint_cache: Default::default(),
            render_line_cache: Default::default(),
            minimap_cache: Default::default(),
            cached_visible_line_count: None,
            cached_wrapped_total: None,
            cached_row_offsets: None,
            scroll_past_end: true,
            wrap_cache: WrapCache::default(),
            wrap_cache_version: u64::MAX,
            wrap_cache_line_count: 0,
            wrap_dirty_range: None,
            wrap_revision: 0,
            cursor_position_stack: VecDeque::new(),
            selection_history: VecDeque::new(),
            selection_history_source: None,
            cached_line_ending: "LF",
            bg_loading: false,
            pending_cursor_position: None,
            pending_cursor_utf16: false,
            pending_navigation_selection: None,
            pending_session_restore: None,
            blame: Default::default(),
            snippet_tab_stops: Vec::new(),
            snippet_active_stop: 0,
            snippet_active: false,
            snippet_rope: None,
            ec_trim_trailing: None,
            ec_insert_final_newline: None,
        }
    }

    /// Create an editor pre-loaded with the given text (test helper).
    #[cfg(test)]
    pub(crate) fn test_with_text(text: &str) -> Self {
        let mut ed = Self::new();
        ed.rope = Rope::from_str(text);
        ed
    }

    /// Create a placeholder editor that will be filled by background file loading.
    pub(crate) fn new_loading(path: &Path, file_size: u64) -> Self {
        let mut ed = Self::new();
        let loading_message = if file_size == 0 {
            "Loading file...".to_string()
        } else if file_size > 1024 * 1024 {
            format!("{:.1} MB", file_size as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.1} KB", file_size as f64 / 1024.0)
        };
        ed.rope = if file_size == 0 {
            Rope::from_str(&loading_message)
        } else {
            Rope::from_str(&format!("Loading file ({loading_message})..."))
        };
        ed.file_path = Some(path.to_path_buf());
        ed.file_size = file_size;
        ed.read_only = true;
        ed.bg_loading = true;
        ed
    }

    pub(crate) fn open_file(&mut self, path: &Path) -> io::Result<()> {
        crate::editor_document::edit!(self, {
        self.fold_hint_cache.get_mut().clear();
        self.minimap_cache.get_mut().clear();
        // Synchronous opens must use the same BOM-before-binary
        // detection as background opens and reloads. UTF-16 contains NUL
        // bytes even in ordinary source files.
        let loaded = load_text_file(path)?;
        let file_size = loaded.file_size;
        self.rope = loaded.rope;
        self.encoding = loaded.encoding;
        if self.encoding == "Binary" { self.read_only = true; }

        self.file_path = Some(path.to_path_buf());
        self.file_size = file_size;
        self.bg_loading = false;
        self.disk_unavailable = false;
        self.load_failed = false;
        self.rich_features_timed_out = false;
        self.update_large_file_mode(LARGE_FILE_THRESHOLD);
        self.content_version = 0;
        self.mark_syntax_dirty_range(0, self.rope.len_lines());
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.desired_col = None;
        self.scroll_offset = 0;
        self.wrap_scroll_offset = 0;
        self.horizontal_scroll = 0;
        self.modified = false;
        self.selection_anchor = None;
        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.break_undo_group();
        self.cached_visible_line_count = None;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
        // Rope clones share their backing tree. Keeping the saved snapshot is
        // O(1) here and preserves diff/undo behavior at every file size.
        self.saved_rope = Some(self.rope.clone());
        self.saved_encoding = self.encoding;
        self.saved_save_options = None;
        self.reset_unsaved_diff_cache();
        self.last_disk_modified = loaded.last_disk_modified;
        self.disk_unavailable = false;
        self.detect_indentation();
        self.detect_line_ending();
        self.apply_editorconfig();
        Ok(())
            })
    }

    /// Apply .editorconfig settings for the current file, overriding auto-detected values.
    fn apply_editorconfig(&mut self) {
        let path = match &self.file_path {
            Some(p) => p.clone(),
            None => return,
        };
        let ec = crate::editorconfig::resolve(&path);
        if let Some(style) = ec.indent_style {
            self.use_tabs = style == crate::editorconfig::IndentStyle::Tab;
        }
        if let Some(size) = ec.effective_tab_width() {
            if size > 0 && size <= 16 {
                self.tab_size = size;
            }
        }
        if let Some(eol) = ec.end_of_line {
            self.cached_line_ending = match eol {
                crate::editorconfig::EndOfLine::Lf => "LF",
                crate::editorconfig::EndOfLine::Crlf => "CRLF",
                crate::editorconfig::EndOfLine::Cr => "LF", // CR-only not widely supported, fall back
            };
        }
        // Store trim/newline settings on the editor for use during save
        if let Some(v) = ec.trim_trailing_whitespace {
            self.ec_trim_trailing = Some(v);
        }
        if let Some(v) = ec.insert_final_newline {
            self.ec_insert_final_newline = Some(v);
        }
    }

    /// Refresh cached state after file content is replaced (e.g. hot-reload from disk).
    /// Preserves cursor position but clears undo/caches/folds since line numbering may change.
    /// Apply content loaded by a background thread.
    pub(crate) fn apply_bg_loaded(
        &mut self,
        rope: Rope,
        encoding: &'static str,
        file_size: u64,
        large_file_threshold_bytes: u64,
    ) {
        let was_loading=self.bg_loading;
        crate::editor_document::edit!(self, {
        if was_loading && !self.bg_loading {self.document.restore_view_only=true;return;}

        self.fold_hint_cache.get_mut().clear();
        self.rope = rope;
        self.minimap_cache.get_mut().clear();
        self.encoding = encoding;
        self.file_size = file_size;
        self.rich_features_timed_out = false;
        self.update_large_file_mode(large_file_threshold_bytes);
        self.content_version = 0;
        self.mark_syntax_dirty_range(0, self.rope.len_lines());
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.desired_col = None;
        self.scroll_offset = 0;
        self.wrap_scroll_offset = 0;
        self.horizontal_scroll = 0;
        self.modified = false;
        self.selection_anchor = None;
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.break_undo_group();
        self.cached_visible_line_count = None;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
        self.clear_git_line_status();
        self.saved_rope = Some(self.rope.clone());
        self.saved_encoding = self.encoding;
        self.saved_save_options = None;
        self.reset_unsaved_diff_cache();
        self.detect_indentation();
        self.detect_line_ending();
        self.bg_loading = false;
        self.disk_unavailable = false;
        self.load_failed = false;
            })
    }

    pub(crate) fn update_large_file_mode(&mut self, threshold_bytes: u64) {
        let content_bytes = self.file_size.max(self.rope.len_bytes() as u64);
        self.large_file_mode = content_bytes > threshold_bytes || self.rich_features_timed_out;
        if self.large_file_mode {
            self.pause_large_file_features();
        }
    }

    pub(crate) fn pause_large_file_features(&mut self) {
        // Wrapping and folding can require a whole-document row map. Apply
        // this to shared views too when their document crosses the threshold.
        self.word_wrap = false;
        self.wrap_scroll_offset = 0;
        self.cached_wrapped_total = None;
        self.cached_row_offsets = None;
        self.folded_ranges.clear();
        self.cached_visible_line_count = None;
        self.clear_git_line_status();
        self.lsp_folding_ranges.clear();
    }

    /// Finish restoring a session tab after its disk baseline was loaded on a
    /// worker. This method only touches the matching placeholder, so two tabs
    /// for the same file retain independent cursor and scroll state.
    pub(crate) fn apply_pending_session_restore(&mut self, threshold_bytes: u64) {
        crate::editor_document::edit!(self, {
        let Some(restore) = self.pending_session_restore.take() else {
            return;
        };
        let mut saved = restore.tab;
        if std::mem::take(&mut self.document.restore_view_only) {
            saved.pending_unsaved_content=None;saved.undo_entries=None;saved.redo_entries=None;
        }

        if let Some(content) = saved.pending_unsaved_content {
            self.fold_hint_cache.get_mut().clear();
            self.minimap_cache.get_mut().clear();
            self.rope = content;
            self.mark_syntax_dirty_range(0, self.rope.len_lines());
            self.modified = true;
            self.cached_visible_line_count = None;
            self.cached_wrapped_total = None;
            self.cached_row_offsets = None;
        }
        if let (Some(undo), Some(redo)) = (saved.undo_entries, saved.redo_entries) {
            self.import_undo_redo(undo, redo);
        }
        if self.saved_rope.as_ref().is_some_and(|rope| self.rope.is_instance(rope)) {
            self.refresh_modified_from_saved();
        }
        self.language_override = saved.language_override;
        self.cursor_line = saved
            .cursor_line
            .min(self.rope.len_lines().saturating_sub(1));
        self.cursor_col = saved.cursor_col.min(self.line_len(self.cursor_line));
        self.scroll_offset = saved
            .scroll_offset
            .min(self.rope.len_lines().saturating_sub(1));
        self.horizontal_scroll = saved.horizontal_scroll;
        self.bookmarks = saved
            .bookmarks
            .into_iter()
            .filter(|&line| line < self.rope.len_lines())
            .collect();
        self.folded_ranges.clear();
        for (start, end) in saved.folded_ranges {
            if start < self.rope.len_lines() && end < self.rope.len_lines() {
                self.folded_ranges.insert(start, end);
            }
        }
        self.update_large_file_mode(threshold_bytes);
            })
    }

    pub(crate) fn mark_rich_features_timed_out(&mut self, threshold_bytes: u64) {
        self.rich_features_timed_out = true;
        self.update_large_file_mode(threshold_bytes);
    }

    /// Analyze the first 100 lines to auto-detect indentation style (tabs vs spaces, width).
    pub(crate) fn detect_indentation(&mut self) {
        // Automatic style detection is a sample, not a document operation.
        // Bound each sample too: a log may begin with a gigabyte-long blank
        // line, and background-load installation runs on the UI thread.
        const MAX_INDENT_SAMPLE_CHARS: usize = 256;
        let mut tab_count = 0usize;
        let mut space_counts = [0usize; 5]; // index 0=2spaces, 1=3spaces, 2=4spaces, 3=6spaces, 4=8spaces
        let line_count = self.rope.len_lines().min(100);
        for i in 0..line_count {
            let line = self.rope.line(i);
            let first_char = line.chars().next();
            if first_char == Some('\t') {
                tab_count += 1;
            } else if first_char == Some(' ') {
                let spaces = line.chars().take(MAX_INDENT_SAMPLE_CHARS + 1)
                    .take_while(|c| *c == ' ').count();
                if spaces > MAX_INDENT_SAMPLE_CHARS { continue; }
                let line_char_count = line.len_chars();
                // Exclude trailing newline from char count comparison
                let trailing_newlines =
                    if line_char_count > 0 && line.char(line_char_count - 1) == '\n' {
                        if line_char_count > 1 && line.char(line_char_count - 2) == '\r' {
                            2
                        } else {
                            1
                        }
                    } else {
                        0
                    };
                let content_chars = line_char_count - trailing_newlines;
                if spaces >= 2 && spaces < content_chars {
                    if spaces.is_multiple_of(8) {
                        space_counts[4] += 1; // 8
                    }
                    if spaces.is_multiple_of(4) {
                        space_counts[2] += 1; // 4
                    }
                    if spaces.is_multiple_of(2) {
                        space_counts[0] += 1; // 2
                    }
                }
            }
        }
        let total_space: usize = space_counts.iter().sum();
        if tab_count > total_space && tab_count >= 3 {
            self.use_tabs = true;
        } else if total_space >= 3 {
            self.use_tabs = false;
            // Prefer 4 > 2 > 8 by checking specificity
            if space_counts[2] > space_counts[0] / 2 {
                self.tab_size = 4;
            } else if space_counts[0] >= 3 {
                self.tab_size = 2;
            } else if space_counts[4] >= 3 {
                self.tab_size = 8;
            }
        }
    }

    /// Detect line ending from first few lines (CRLF vs LF). Called once on file load.
    pub(crate) fn detect_line_ending(&mut self) {
        self.cached_line_ending = rope_line_ending(&self.rope);
    }

    pub(crate) fn save_file_with_options(
        &mut self,
        trim_trailing: bool,
        insert_final_newline: bool,
    ) -> io::Result<()> {
        crate::editor_document::edit!(self, {
        if let Some(error) = self.save_content_error() {
            return Err(io::Error::other(error));
        }
        let request = self.background_save_request(trim_trailing, insert_final_newline, false)
            .ok_or_else(|| io::Error::other("No file to save"))?;
        let saved = write_background_save(&request)?;
        // Preparation stays separate: a failed save must not change history.
        if let Some((first, last)) = saved.transformed_lines {
            self.save_undo_for_range(first, last);
            let old_lines = self.rope.len_lines();
            self.rope = saved.rope;
            self.mark_syntax_dirty_range(first, last);
            self.snippet_cancel();
            if old_lines != self.rope.len_lines() { self.clear_stale_folds(); }
            self.set_multi_cursor_states(self.collect_multi_cursors());
        }
        self.break_undo_group();
        if let Some(metadata) = saved.metadata {
            self.file_size = metadata.len();
            self.last_disk_modified = metadata.modified().ok();
        }
        self.record_saved_snapshot(self.rope.clone());
        self.disk_unavailable = false;
        Ok(())
            })
    }

    fn disk_save_baseline(&self) -> Option<DiskSaveBaseline> {
        self.saved_rope.as_ref().map(|rope| DiskSaveBaseline {
            rope: rope.clone(), encoding: self.saved_encoding,
        })
    }

    #[cfg(test)]
    fn write_encoded_content(&self, rope: &Rope, writer: &mut impl Write) -> io::Result<()> {
        save::write_encoded(rope, self.encoding, writer)
    }

    #[cfg(test)]
    fn prepared_save_rope(&self, trim: bool, final_newline: bool) -> Rope {
        save::prepare_rope(&self.rope, trim, final_newline, self.cached_line_ending == "CRLF")
    }

    #[cfg(test)]
    fn prepare_save_content(&mut self, trim_trailing: bool, insert_final_newline: bool) {
        crate::editor_document::edit!(self, {
        let prepared = self.prepared_save_rope(trim_trailing, insert_final_newline);
        if !crate::rope_changes::equal(&self.rope, &prepared) {
            self.rope = prepared;
            self.mark_syntax_dirty_range(0, self.rope.len_lines());
            self.content_version += 1;
            self.cached_visible_line_count = None;
            self.cached_wrapped_total = None;
            self.cached_row_offsets = None;
            self.set_multi_cursor_states(self.collect_multi_cursors());
        }
            })
    }

    pub(crate) fn set_read_only(&mut self, read_only: bool) {
        crate::editor_document::edit!(self, {
            // The user preference cannot turn loading/error/binary messages
            // into editable documents.
            self.read_only = read_only || self.save_content_error().is_some();
        })
    }

    /// UI placeholders must never become the contents of a saved file.
    pub(crate) fn save_content_error(&self) -> Option<&'static str> {
        if self.bg_loading {
            Some("The file is still loading. Wait for it to finish before saving.")
        } else if self.load_failed {
            Some("The file could not be opened. Reopen it before saving.")
        } else if self.encoding == "Binary" {
            Some("Binary files cannot be saved as text.")
        } else {
            None
        }
    }

    /// Capture a constant-time background save request. Unlike the previous
    /// implementation, this does not scan/transform the Rope or probe metadata
    /// on the caller's thread.
    pub(crate) fn background_save_request(
        &self,
        trim_trailing: bool,
        insert_final_newline: bool,
        fail_if_exists: bool,
    ) -> Option<BackgroundSaveRequest> {
        if self.save_content_error().is_some() { return None; }
        let path = self.file_path.clone()?;
        Some(BackgroundSaveRequest {
            source_document_id: self.save_identity,
            path,
            rope: self.rope.clone(),
            encoding: self.encoding,
            line_ending: self.cached_line_ending,
            trim_trailing: self.ec_trim_trailing.unwrap_or(trim_trailing),
            insert_final_newline: self.ec_insert_final_newline.unwrap_or(insert_final_newline),
            source_content_version: self.content_version,
            fail_if_exists: fail_if_exists || self.saved_rope.is_none(),
            expected_disk: self.disk_save_baseline(),
            prepared_options: self.saved_save_options,
        })
    }

    pub(crate) fn save_options(&self, trim: bool, newline: bool) -> SaveOptions {
        SaveOptions { trim_trailing: self.ec_trim_trailing.unwrap_or(trim),
            insert_final_newline: self.ec_insert_final_newline.unwrap_or(newline), line_ending: self.cached_line_ending }
    }

    pub(crate) fn record_save_options(&mut self, options: Option<SaveOptions>) {
        crate::editor_document::edit!(self, { self.saved_save_options = options; })
    }

    /// Reconcile a completed background save with the live editor. If typing
    /// continued while the write was in flight, the live Rope remains intact
    /// and modified; only its on-disk baseline advances.
    pub(crate) fn apply_background_save(
        &mut self,
        source_content_version: u64,
        source_rope: &Rope,
        saved_rope: Rope,
        saved_encoding: &'static str,
        last_disk_modified: Option<std::time::SystemTime>,
        file_size: Option<u64>,
        transformed_lines: Option<(usize, usize)>,
        large_file_threshold_bytes: u64,
    ) {
        crate::editor_document::edit!(self, {
        // Versions are local counters, reused by a reopened file or another
        // view. Only the actual submitted snapshot can authorize replacement.
        let still_current = self.content_version == source_content_version
            && crate::rope_changes::equal(&self.rope, source_rope);
        let saved_rope = if still_current && transformed_lines.is_none() {
            self.rope.clone()
        } else {
            saved_rope
        };
        if still_current {
            self.break_undo_group();
            if let Some((first, last)) = transformed_lines {
                self.save_undo_for_range(first, last);
                let old_lines = self.rope.len_lines();
                self.rope = saved_rope.clone();
                self.mark_syntax_dirty_range(first, last);
                self.cached_visible_line_count = None;
                self.cached_wrapped_total = None;
                self.snippet_cancel();
                if old_lines != self.rope.len_lines() { self.clear_stale_folds(); }
                self.set_multi_cursor_states(self.collect_multi_cursors());
            }
        }
        self.record_saved_snapshot_with_encoding(saved_rope, saved_encoding);
        if !self.saved_rope.as_ref().is_some_and(|saved| self.rope.is_instance(saved)) {
            self.invalidate_unsaved_diff_cache();
        }
        self.last_disk_modified = last_disk_modified;
        self.disk_unavailable = false;
        if let Some(file_size) = file_size {
            self.file_size = file_size;
        }
        self.update_large_file_mode(large_file_threshold_bytes);
            })
    }

    pub(crate) fn line_count(&self) -> usize {
        self.rope.len_lines().max(1)
    }

    pub(crate) fn unsaved_line_status(&self, line: usize) -> char {
        if !self.modified || self.saved_rope.is_none() {
            return ' ';
        }

        // Use the exact worker-produced diff when it matches this buffer.
        if self.unsaved_diff_cache.0 == self.content_version {
            return self.unsaved_diff_cache.1.line_status(line);
        }

        // Keep gutter feedback immediate for the lines currently being edited.
        // The worker will replace these provisional markers with an exact diff.
        if line == self.cursor_line
            || self
                .extra_cursors
                .iter()
                .any(|&(cursor_line, _)| cursor_line == line)
        {
            return 'N';
        }

        // Reuse the last exact markers while the next diff is in flight. For a
        // line-count edit, the accumulated edit range gives us a safe unchanged
        // prefix and a suffix shifted by the total line delta. Keeping those
        // rows avoids the gutter blinking blank on newline undo/redo.
        self.stale_unsaved_source_line(line)
            .map(|source| self.unsaved_diff_cache.1.line_status(source))
            .unwrap_or(' ')
    }

    pub(crate) fn apply_unsaved_diff_cache(
        &mut self,
        version: u64,
        changes: UnsavedDiff,
    ) {
        if self.content_version == version {
            self.unsaved_diff_cache = (version, changes);
            self.unsaved_diff_dirty_from = None;
            self.unsaved_diff_dirty_through = None;
            self.overview_change_revision = self.overview_change_revision.wrapping_add(1);
        }
    }

    /// Retire exact markers without discarding an already-known edit range.
    pub(crate) fn invalidate_unsaved_diff_version(&mut self) {
        self.unsaved_diff_cache.0 = u64::MAX;
    }

    pub(crate) fn invalidate_unsaved_diff_cache(&mut self) {
        self.invalidate_unsaved_diff_version();
        self.unsaved_diff_dirty_from = Some(0);
        self.unsaved_diff_dirty_through = Some(self.line_count().saturating_sub(1));
        // Keep the last exact display data until the worker publishes the new
        // version. The marker payload did not change, so do not invalidate the
        // already bucketed minimap rows here: doing so made every keystroke
        // rescan and sort tens of thousands of stale markers on the UI thread.
    }

    pub(crate) fn reset_unsaved_diff_cache(&mut self) {
        self.unsaved_diff_cache = (self.content_version, UnsavedDiff::default());
        self.unsaved_diff_dirty_from = None;
        self.unsaved_diff_dirty_through = None;
        self.overview_change_revision = self.overview_change_revision.wrapping_add(1);
    }

    fn stale_unsaved_source_line(&self, current_line: usize) -> Option<usize> {
        let old_line_count = self.unsaved_diff_cache.1.line_count;
        let current_line_count = self.line_count();
        if old_line_count == 0 || current_line >= current_line_count {
            return None;
        }
        if old_line_count == current_line_count {
            return Some(current_line);
        }

        let dirty_from = self.unsaved_diff_dirty_from.unwrap_or(0);
        let dirty_through = self
            .unsaved_diff_dirty_through
            .unwrap_or_else(|| current_line_count.saturating_sub(1));
        if current_line < dirty_from {
            return (current_line < old_line_count).then_some(current_line);
        }
        if current_line <= dirty_through {
            return None;
        }

        let source = current_line as i128 - (current_line_count as i128 - old_line_count as i128);
        (source >= 0 && source < old_line_count as i128).then_some(source as usize)
    }

    fn sorted_unsaved_line_in_range(&self, start: usize, end: usize) -> bool {
        self.unsaved_diff_cache.1.has_change_in_range(start, end)
    }

    /// Query a minimap-sized line bucket without materializing projected
    /// markers. This is O(log changed-lines), even if a 50k-line document has
    /// every line marked, and retains safe stale rows until analysis catches up.
    pub(crate) fn has_unsaved_change_in_range(&self, start: usize, end: usize) -> bool {
        if !self.modified || self.saved_rope.is_none() || start >= end {
            return false;
        }
        if self.unsaved_diff_cache.0 == self.content_version {
            return self.sorted_unsaved_line_in_range(start, end);
        }
        if (start..end).contains(&self.cursor_line)
            || self
                .extra_cursors
                .iter()
                .any(|&(line, _)| (start..end).contains(&line))
        {
            return true;
        }
        let old_line_count = self.unsaved_diff_cache.1.line_count;
        let current_line_count = self.line_count();
        if old_line_count == 0 {
            return false;
        }
        let dirty_from = self.unsaved_diff_dirty_from.unwrap_or(0);
        let dirty_through = self
            .unsaved_diff_dirty_through
            .unwrap_or_else(|| current_line_count.saturating_sub(1));

        let prefix_end = end.min(dirty_from).min(old_line_count);
        if self.sorted_unsaved_line_in_range(start.min(prefix_end), prefix_end) {
            return true;
        }

        let suffix_start = start.max(dirty_through.saturating_add(1));
        if suffix_start >= end {
            return false;
        }
        let delta = current_line_count as i128 - old_line_count as i128;
        let source_start = (suffix_start as i128 - delta).clamp(0, old_line_count as i128);
        let source_end = (end as i128 - delta).clamp(0, old_line_count as i128);
        self.sorted_unsaved_line_in_range(source_start as usize, source_end as usize)
    }

    pub(crate) fn set_git_line_status(&mut self, statuses: Vec<char>) -> bool {
        self.set_prepared_git_line_status(PreparedGitLineStatus::from_markers(statuses))
    }

    pub(crate) fn set_prepared_git_line_status(
        &mut self,
        status: PreparedGitLineStatus,
    ) -> bool {
        if self.git_line_status.len() == status.markers.len()
            && self.git_line_status_signature == status.signature
        {
            return false;
        }
        self.git_line_status = status.markers;
        self.git_change_lines = status.changes;
        self.git_line_status_signature = status.signature;
        self.overview_change_revision = self.overview_change_revision.wrapping_add(1);
        true
    }

    pub(crate) fn clear_git_line_status(&mut self) {
        if self.git_line_status.is_empty() && self.git_change_lines.is_empty() {
            return;
        }
        self.git_line_status = Arc::from([]);
        self.git_change_lines = Arc::from([]);
        self.git_line_status_signature = 0;
        self.overview_change_revision = self.overview_change_revision.wrapping_add(1);
    }

    pub(crate) fn git_change_lines(&self) -> &[(usize, char)] {
        &self.git_change_lines
    }

    pub(crate) fn git_line_status(&self) -> &[char] {
        &self.git_line_status
    }

    pub(crate) fn git_line_status_len(&self) -> usize {
        self.git_line_status.len()
    }

    pub(crate) fn has_git_line_changes(&self) -> bool {
        !self.git_change_lines.is_empty()
    }

    pub(crate) fn prepared_git_line_status(&self) -> PreparedGitLineStatus {
        PreparedGitLineStatus {
            markers: Arc::clone(&self.git_line_status),
            changes: Arc::clone(&self.git_change_lines),
            signature: self.git_line_status_signature,
        }
    }

    /// Returns per-line change status:
    /// 'N' = unsaved buffer change, 'U' = unstaged git change, 'D' = deleted-line marker, 'S' = staged git change, ' ' = unchanged.
    pub(crate) fn line_diff_status(&self, line: usize) -> char {
        let unsaved = self.unsaved_line_status(line);
        if unsaved != ' ' {
            return unsaved;
        }
        if !self.git_line_status.is_empty() {
            return self.git_line_status.get(line).copied().unwrap_or(' ');
        };
        ' '
    }

    pub(crate) fn current_line_len(&self) -> usize {
        self.line_len(self.cursor_line)
    }

    /// Accumulate the full line range an upcoming/just-applied edit can affect.
    pub(crate) fn mark_syntax_dirty_range(&mut self, start: usize, end: usize) {
        self.wrap_revision = self.wrap_revision.wrapping_add(1);
        self.wrap_dirty_range = Some(self.wrap_dirty_range.map_or((start, end), |(from, through)| {
            (from.min(start), through.max(end))
        }));
        self.render_line_cache.get_mut().mark_dirty(
            start,
            end,
            self.content_version,
            self.wrap_revision,
            &self.rope,
        );
        self.fold_hint_cache.get_mut().mark_dirty(start, end);
        self.minimap_cache.get_mut().mark_dirty(start, end);
        let last_line = self.rope.len_lines().saturating_sub(1);
        let start = start.min(last_line);
        let end = end.max(start).min(last_line);
        self.syntax_dirty_from = Some(
            self.syntax_dirty_from
                .map_or(start, |previous| previous.min(start)),
        );
        self.syntax_dirty_through = Some(
            self.syntax_dirty_through
                .map_or(end, |previous| previous.max(end)),
        );
        self.unsaved_diff_dirty_from = Some(
            self.unsaved_diff_dirty_from
                .map_or(start, |previous| previous.min(start)),
        );
        self.unsaved_diff_dirty_through = Some(
            self.unsaved_diff_dirty_through
                .map_or(end, |previous| previous.max(end)),
        );

        // `save_undo` starts recording the edit represented by its newest
        // snapshot. Later marks (including grouped typing and whole-document
        // transforms) widen that snapshot's range. Cursor movement breaks the
        // recording, so a future edit cannot accidentally widen an older one.
        if let Some(active_id) = self.active_undo_change_id {
            if let Some(mut state) = self
                .undo_stack
                .back_mut()
                .filter(|state| state.change_id == active_id)
            {
                state.dirty_from = state.dirty_from.min(start);
                state.dirty_through = state.dirty_through.max(end);
            }
        }
    }

    /// Selection anchors and secondary cursors can be above the primary cursor;
    /// compute the true earliest possible mutation before changing the Rope.
    pub(crate) fn earliest_edit_line(&self) -> usize {
        let mut line = self.cursor_line;
        if let Some((anchor_line, _)) = self.selection_anchor {
            line = line.min(anchor_line);
        }
        for &(cursor_line, _) in &self.extra_cursors {
            line = line.min(cursor_line);
        }
        for &(anchor_line, _) in self.extra_cursor_anchors.iter().flatten() {
            line = line.min(anchor_line);
        }
        line
    }

    pub(crate) fn latest_edit_line(&self) -> usize {
        let mut line = self.cursor_line;
        if let Some((anchor_line, _)) = self.selection_anchor {
            line = line.max(anchor_line);
        }
        for &(cursor_line, _) in &self.extra_cursors {
            line = line.max(cursor_line);
        }
        for &(anchor_line, _) in self.extra_cursor_anchors.iter().flatten() {
            line = line.max(anchor_line);
        }
        line
    }

    pub(crate) fn ensure_visible(&mut self) {
        // Auto-unfold if cursor is inside a folded region
        if self.is_line_folded(self.cursor_line) {
            let to_unfold: Vec<usize> = self
                .folded_ranges
                .iter()
                .filter(|&(&s, &e)| self.cursor_line > s && self.cursor_line < e)
                .map(|(&s, _)| s)
                .collect();
            for start in to_unfold {
                self.folded_ranges.remove(&start);
            }
            self.cached_wrapped_total = None;
            self.cached_row_offsets = None;
            self.cached_visible_line_count = None;
        }

        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let visible_rows = self.viewport_height.max(1);
            let cursor_wrap_row =
                self.wrapped_row_for_position(self.cursor_line, self.cursor_col, wrap_width);
            if self.typewriter_mode && visible_rows > 2 {
                let center = visible_rows / 2;
                self.wrap_scroll_offset = cursor_wrap_row.saturating_sub(center);
            } else {
                if cursor_wrap_row < self.wrap_scroll_offset {
                    self.wrap_scroll_offset = cursor_wrap_row;
                }
                let max_visible_row = self
                    .wrap_scroll_offset
                    .saturating_add(visible_rows.saturating_sub(1));
                if cursor_wrap_row > max_visible_row {
                    self.wrap_scroll_offset =
                        cursor_wrap_row.saturating_sub(visible_rows.saturating_sub(1));
                }
            }
            let max_wrap_row = self.wrapped_total_rows(wrap_width).saturating_sub(1);
            self.wrap_scroll_offset = self.wrap_scroll_offset.min(max_wrap_row);
            self.sync_line_scroll_from_wrap(wrap_width);
            self.horizontal_scroll = 0;
            return;
        }

        if self.typewriter_mode && self.viewport_height > 2 {
            // Center cursor vertically
            let center = self.viewport_height / 2;
            self.scroll_offset = self.cursor_line.saturating_sub(center);
        } else {
            // Normal scrolling with surrounding lines margin
            let margin = self.cursor_surrounding_lines.min(self.viewport_height / 2);
            if self.cursor_line < self.scroll_offset + margin {
                self.scroll_offset = self.cursor_line.saturating_sub(margin);
            }
            let visible = self.viewport_height.saturating_sub(1);
            if visible > margin && self.cursor_line + margin >= self.scroll_offset + visible {
                self.scroll_offset = (self.cursor_line + margin).saturating_sub(visible) + 1;
            }
        }
        // Clamp scroll offset so we don't scroll past end of file
        let max_scroll = if self.scroll_past_end {
            self.line_count().saturating_sub(1)
        } else {
            self.line_count()
                .saturating_sub(self.viewport_height.max(1))
        };
        self.scroll_offset = self.scroll_offset.min(max_scroll);
        // Horizontal scrolling with small margin
        let h_margin: usize = 4;
        if self.cursor_col < self.horizontal_scroll + h_margin {
            self.horizontal_scroll = self.cursor_col.saturating_sub(h_margin);
        }
        let h_visible = self.viewport_width.saturating_sub(1);
        if h_visible > h_margin && self.cursor_col + h_margin >= self.horizontal_scroll + h_visible
        {
            self.horizontal_scroll = (self.cursor_col + h_margin).saturating_sub(h_visible) + 1;
        }
        self.reveal_cursor_cells();
        self.wrap_scroll_offset = self.scroll_offset;
    }

    pub(crate) fn set_cursor(&mut self, line: usize, col: usize) {
        self.set_cursor_without_scrolling(line, col);
        if !self.bg_loading {
            self.ensure_visible();
        }
    }

    /// Mouse hit testing already chose a visible position. Keyboard scroll
    /// margins must not move that text out from under a click or drag.
    pub(crate) fn set_cursor_without_scrolling(&mut self, line: usize, col: usize) {
        self.pending_cursor_utf16 = false;
        if self.bg_loading {
            self.pending_cursor_position = Some((line, col));
            self.pending_navigation_selection = None;
            return;
        }
        self.desired_col = None;
        self.break_undo_group();
        self.cursor_line = line.min(self.line_count().saturating_sub(1));
        self.cursor_col = col.min(self.line_len(self.cursor_line));
    }

    pub(crate) fn file_name(&self) -> &str {
        self.file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .or(self.suggested_file_name.as_deref())
            .unwrap_or("[No File]")
    }

    /// UI label for unnamed documents; keep file_name() for language detection,
    /// file icons and save paths. Reading a bounded prefix avoids copying long
    /// first lines whenever the tab bar is painted or hit-tested.
    pub(crate) fn display_name(&self) -> Cow<'_, str> {
        use unicode_segmentation::UnicodeSegmentation;

        if let Some(name) = self.file_path.as_deref().and_then(crate::remote::workspace::display_name) {
            return Cow::Owned(name);
        }
        if self.file_path.is_some() || self.suggested_file_name.is_some() {
            return Cow::Borrowed(self.file_name());
        }
        let line = self.rope.line(0);
        let end = line.len_chars().min(128);
        let prefix = line.slice(..end);
        let text = prefix.as_str().map_or_else(
            || Cow::Owned(prefix.to_string()), Cow::Borrowed,
        );
        let clipped = end < line.len_chars();
        // A grapheme at the sample boundary may continue in the next chunk.
        let complete = if clipped {
            let end = text.grapheme_indices(true).next_back().map_or(0, |(end, _)| end);
            &text[..end]
        } else {
            text.as_ref()
        };
        let complete = complete.trim();
        if complete.is_empty() {
            return Cow::Borrowed("[NoFile]");
        }
        let mut chars = complete.graphemes(true).filter(|g| {
            !g.chars().all(|c| c.is_control() && !c.is_whitespace())
        });
        let mut title = String::with_capacity(48);
        for ch in chars.by_ref().take(10) {
            if ch.chars().all(char::is_whitespace) {
                title.push(' ');
            } else {
                title.push_str(ch);
            }
        }
        title.truncate(title.trim_end().len());
        if title.is_empty() {
            return Cow::Borrowed("[NoFile]");
        }
        if clipped || chars.next().is_some() {
            title.push('…');
        }
        title.push_str(" [NoFile]");
        Cow::Owned(title)
    }

    /// Returns (total_lines, non_blank_lines, words, chars).
    pub(crate) fn compute_file_stats(&self) -> (usize, usize, usize, usize) {
        let lines = self.line_count();
        let chars = self.rope.len_chars();
        let mut words = 0usize;
        let mut non_blank = 0usize;
        let mut in_word = false;
        let mut has_non_ws = false;
        for chunk in self.rope.chunks() {
            for ch in chunk.chars() {
                if ch == '\n' {
                    if has_non_ws {
                        non_blank += 1;
                    }
                    has_non_ws = false;
                    in_word = false;
                } else if ch.is_whitespace() {
                    in_word = false;
                } else {
                    has_non_ws = true;
                    if !in_word {
                        words += 1;
                    }
                    in_word = true;
                }
            }
        }
        // Count last line if it has non-whitespace content
        if has_non_ws {
            non_blank += 1;
        }
        (lines, non_blank, words, chars)
    }

    /// Preview eligibility follows explicit languages and file associations
    /// before falling back to the supported Markdown filename extensions.
    pub(crate) fn is_markdown(&self) -> bool {
        if let Some(language) = self.syntax_language_token() {
            return language.eq_ignore_ascii_case("Markdown");
        }
        self.file_name().rsplit('.').next().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("md")
                || extension.eq_ignore_ascii_case("markdown")
                || extension.eq_ignore_ascii_case("mdx")
        })
    }

    pub(crate) fn language_name(&self) -> &str {
        if let Some(language) = self.syntax_language_token() {
            return language;
        }
        let ext = self
            .file_path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("");

        // Check filename first for special files
        let filename = self
            .file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|f| f.to_str())
            .unwrap_or("");

        match filename {
            "Makefile" | "makefile" | "GNUmakefile" => return "Makefile",
            "Dockerfile" => return "Dockerfile",
            "CMakeLists.txt" => return "CMake",
            ".gitignore" | ".gitattributes" => return "Git Config",
            ".editorconfig" => return "EditorConfig",
            ".env" | ".env.local" | ".env.production" => return "Env",
            "Cargo.toml" => return "TOML (Cargo)",
            "package.json" | "tsconfig.json" => return "JSON",
            _ => {}
        }

        match ext {
            "rs" => "Rust",
            "py" | "pyw" | "pyi" => "Python",
            "js" | "mjs" | "cjs" => "JavaScript",
            "ts" | "mts" | "cts" => "TypeScript",
            "tsx" => "TypeScript React",
            "jsx" => "JavaScript React",
            "json" | "jsonc" | "json5" => "JSON",
            "toml" => "TOML",
            "yaml" | "yml" => "YAML",
            "md" | "markdown" | "mdx" => "Markdown",
            "html" | "htm" | "xhtml" => "HTML",
            "css" => "CSS",
            "scss" => "SCSS",
            "sass" => "Sass",
            "less" => "Less",
            "go" => "Go",
            "java" => "Java",
            "kt" | "kts" => "Kotlin",
            "scala" | "sc" => "Scala",
            "c" => "C",
            "cpp" | "cc" | "cxx" | "c++" | "C" => "C++",
            "h" | "hpp" | "hxx" | "hh" | "h++" | "H" => "C/C++ Header",
            "cs" | "csx" => "C#",
            "fs" | "fsx" => "F#",
            "rb" | "erb" => "Ruby",
            "sh" | "bash" | "zsh" => "Shell Script",
            "fish" => "Fish",
            "ps1" | "psm1" | "psd1" => "PowerShell",
            "sql" => "SQL",
            "xml" | "xsl" | "xslt" => "XML",
            "svg" => "SVG",
            "lua" => "Lua",
            "tcl" | "tk" | "itcl" | "tm" => "Tcl",
            "hs" | "lhs" => "Haskell",
            "ex" | "exs" => "Elixir",
            "erl" | "hrl" => "Erlang",
            "clj" | "cljs" => "Clojure",
            "r" | "R" => "R",
            "swift" => "Swift",
            "dart" => "Dart",
            "php" => "PHP",
            "pl" | "pm" => "Perl",
            "zig" => "Zig",
            "nim" => "Nim",
            "v" => "V",
            "vue" => "Vue",
            "svelte" => "Svelte",
            "tf" | "tfvars" => "Terraform",
            "hcl" => "HCL",
            "cmake" => "CMake",
            "mk" | "mak" => "Makefile",
            "proto" => "Protocol Buffers",
            "graphql" | "gql" => "GraphQL",
            "log" => "Log",
            "csv" => "CSV",
            "ini" | "cfg" | "conf" => "INI",
            "lock" => "Lock File",
            "bat" | "cmd" => "Batch",
            "dockerfile" => "Dockerfile",
            "" => {
                // Check shebang line for extensionless files
                if self.rope.len_lines() > 0 {
                    let first_slice = self.rope.line(0);
                    // Untitled/extensionless files ask for their language on
                    // ordinary typing and redraws. Reject non-shebang text
                    // before copying a potentially huge first logical line.
                    let mut prefix = first_slice.chars();
                    if prefix.next() != Some('#') || prefix.next() != Some('!') {
                        return "Plain Text";
                    }
                    let first_line: std::borrow::Cow<str> = match first_slice.as_str() {
                        Some(s) => std::borrow::Cow::Borrowed(s),
                        None => std::borrow::Cow::Owned(first_slice.to_string()),
                    };
                    if first_line.starts_with("#!") {
                        if first_line.contains("python") {
                            return "Python";
                        }
                        if first_line.contains("node")
                            || first_line.contains("deno")
                            || first_line.contains("bun")
                        {
                            return "JavaScript";
                        }
                        if first_line.contains("bash")
                            || first_line.contains("/sh")
                            || first_line.contains("zsh")
                        {
                            return "Shell Script";
                        }
                        if first_line.contains("ruby") {
                            return "Ruby";
                        }
                        if first_line.contains("perl") {
                            return "Perl";
                        }
                        if first_line.contains("php") {
                            return "PHP";
                        }
                        if first_line.contains("lua") {
                            return "Lua";
                        }
                        if first_line.contains("elixir") {
                            return "Elixir";
                        }
                        if first_line.contains("pwsh") || first_line.contains("powershell") {
                            return "PowerShell";
                        }
                    }
                }
                "Plain Text"
            }
            _ => "Plain Text",
        }
    }

    /// Explicit grammar token for Syntect. Ordinary files are best resolved
    /// from their path, while user overrides and language associations should
    /// take precedence over the extension built into a grammar.
    pub(crate) fn syntax_file_path(&self) -> Option<&std::path::Path> {
        self.blame.revision.as_ref().map(|revision| revision.syntax_path.as_path())
            .or(self.file_path.as_deref())
            .or_else(|| self.suggested_file_name.as_deref().map(std::path::Path::new))
    }

    pub(crate) fn syntax_language_token(&self) -> Option<&str> {
        if let Some(language) = self.language_override.as_deref() {
            return Some(language);
        }
        if self.language_associations.is_empty() {
            return None;
        }
        crate::file_associations::resolve(&self.language_associations, self.syntax_file_path()?)
            .map(|(_, language)| language)
    }

    /// Accept one complete conflict as a single shared-document undo step.
    pub(crate) fn resolve_conflict(&mut self, start_line: usize, action: &str) -> bool {
        crate::editor_document::edit!(self, {
            if self.read_only || self.bg_loading || self.load_failed || self.encoding == "Binary"
                || !matches!(action, "current" | "incoming" | "both") {
                return false;
            }
            let region = if let Some(index) = self.merge_conflicts.current(&self.rope) {
                index.region_for_line(start_line).filter(|region| region.start == start_line)
            } else { crate::merge_conflicts::at(&self.rope, start_line) };
            let Some(region) = region else { return false; };
            let chars = |lines| crate::merge_conflicts::line_chars(&self.rope, lines);
            let mut replacement: Rope = match action {
                "incoming" => self.rope.slice(chars(region.incoming())).into(),
                _ => self.rope.slice(chars(region.current())).into(),
            };
            if action == "both" { replacement.append(self.rope.slice(chars(region.incoming())).into()); }
            let remove = chars(region.start..region.end + 1);
            self.break_undo_group();
            self.save_undo_for_range(region.start, region.end);
            self.snippet_cancel();
            let tail = self.rope.split_off(remove.end);
            self.rope.remove(remove.start..);
            self.rope.append(replacement);
            self.rope.append(tail);
            self.cursor_line = region.start.min(self.rope.len_lines() - 1);
            self.cursor_col = 0;
            self.desired_col = None;
            self.selection_anchor = None;
            self.extra_cursors.clear();
            self.extra_cursor_anchors.clear();
            self.clear_stale_folds();
            self.mark_syntax_dirty_range(region.start, region.end);
            self.content_version = self.content_version.wrapping_add(1);
            self.modified = true;
            self.refresh_modified_from_saved();
            self.break_undo_group();
            true
        })
    }

    /// Return (word_start, word_end) column range for the word at (line, col).
    /// If the position is not on a word character, returns (col, col).
    pub(crate) fn word_boundaries_at(&self, line: usize, col: usize) -> (usize, usize) {
        if line >= self.rope.len_lines() {
            return (col, col);
        }
        let rope_line = self.rope.line(line);
        let len = rope_line.len_chars().saturating_sub(
            if rope_line.len_chars() > 0 && rope_line.char(rope_line.len_chars() - 1) == '\n' {
                1
            } else {
                0
            },
        );
        if col >= len {
            return (col, col);
        }
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        if !is_word(rope_line.char(col)) {
            return (col, col);
        }
        let start = col - rope_line.chars_at(col).reversed()
            .take_while(|&c| is_word(c)).count();
        let end = col + rope_line.chars_at(col).take(len - col)
            .take_while(|&c| is_word(c)).count();
        (start, end)
    }

    /// Presence checks need only the preceding character, not a scan and
    /// allocation of the entire prefix (e.g. after every Backspace).
    pub(crate) fn has_word_prefix_at_cursor(&self) -> bool {
        let Some(line) = self.rope.get_line(self.cursor_line) else { return false; };
        self.cursor_col.min(line.len_chars()).checked_sub(1)
            .and_then(|col| line.get_char(col))
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_' || ch == '$')
    }

    /// Get the word prefix before the cursor (for vtcomplete)
    pub(crate) fn word_prefix_at_cursor(&self) -> Option<String> {
        if self.cursor_line >= self.rope.len_lines() {
            return None;
        }
        if self.cursor_col == 0 {
            return None;
        }
        let line = self.rope.line(self.cursor_line);
        let line_len = line.len_chars();
        let col = self.cursor_col.min(line_len);
        if col == 0 {
            return None;
        }

        let is_word_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$';

        let start = col - line.chars_at(col).reversed()
            .take_while(|&c| is_word_char(c)).count();
        if start == col {
            return None;
        }
        let prefix = line.slice(start..col).to_string();
        if prefix.is_empty() {
            None
        } else {
            Some(prefix)
        }
    }

    /// Find definition of the word at cursor (heuristic: looks for fn/struct/class/def/etc.)
    pub(crate) fn find_definition(&self, word: &str) -> Option<(usize, usize)> {
        const PREFIXES: &[&str] = &[
            "fn ", "pub(crate) fn ", "struct ", "pub struct ", "enum ",
            "pub enum ", "trait ", "pub trait ", "impl ", "type ", "pub type ",
            "const ", "pub const ", "static ", "pub static ", "let ",
            "let mut ", "class ", "def ", "function ", "var ", "interface ",
        ];
        for (line_idx, line) in self.rope.lines().enumerate() {
            if let Some(text) = line.as_str() {
                let trimmed = text.trim_start();
                for &prefix in PREFIXES {
                    if trimmed.strip_prefix(prefix).is_some_and(|tail| tail.starts_with(word)) {
                        let indent = text.len() - trimmed.len();
                        return Some((line_idx, text[..indent].chars().count() + prefix.len()));
                    }
                }
            } else {
                // Only inspect the declaration prefix of split rope lines.
                // Copying an entire generated/minified line just to reject its
                // first word made heuristic navigation stall on large files.
                let mut chars = line.chars().peekable();
                let mut indent = 0;
                while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
                    chars.next();
                    indent += 1;
                }
                for &prefix in PREFIXES {
                    if chars.peek().copied() != prefix.chars().next() { continue; }
                    let mut candidate = chars.clone();
                    if prefix.chars().chain(word.chars()).all(|ch| candidate.next() == Some(ch)) {
                        return Some((line_idx, indent + prefix.len()));
                    }
                }
            }
        }
        None
    }

    /// Complete the current word with the given completion
    pub(crate) fn complete_word(&mut self, completion: &str) {
        crate::editor_document::edit!(self, {
        let line = self.rope.line(self.cursor_line);
        let is_word_char = |c: char| c.is_alphanumeric() || c == '_' || c == '$';

        // Seek once and traverse backwards across rope chunks.
        let col = self.cursor_col.min(line.len_chars());
            let before = line
                .chars_at(col)
                .reversed()
                .take(4097)
                .take_while(|&c| is_word_char(c))
                .count();
            let after = line
                .chars_at(col)
                .take(4097)
                .take_while(|&c| is_word_char(c))
                .count();
            // A caret can sit just inside an enormous generated identifier. Tab
            // must never walk its entire suffix on the input thread.
            if before > 4096 || after > 4096 {
                return;
            }
            let start = col - before;
            let end = col + after;
            let callable = completion.ends_with("()");
            let call_spacing = line.chars_at(end)
                .take(16)
            .take_while(|ch| matches!(ch, ' ' | '\t')).count();
            let existing_call = callable && line.get_char(end + call_spacing) == Some('(');
            let insertion = if existing_call {
                &completion[..completion.len() - 2]
            } else {
                completion
            };

        self.save_undo();
        let line_start = self.rope.line_to_char(self.cursor_line);
        let from = line_start + start;
        let to = line_start + end;
        self.rope.remove(from..to);
        self.rope.insert(from, insertion);
        self.cursor_col = start + insertion.chars().count()
                + if existing_call { call_spacing + 1 } else { 0 }
                - usize::from(callable && !existing_call);
        self.modified = true;
        self.ensure_visible();
            })
    }

    /// Numeric predictions replace one literal, including its fractional tail.
    /// Keep signs separate from binary subtraction and reject ambiguous atoms.
    pub(crate) fn complete_number(&mut self, completion: &str) {
        fn numeric_atom(text: &str) -> bool {
            let text = text.strip_prefix('-').unwrap_or(text);
            if text.is_empty() {
                return true; // Empty value or a partially typed unary minus.
            }
            if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
                return hex.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '_');
            }
            let mut parts = text.split('.');
            let whole = parts.next().unwrap();
            whole.starts_with(|ch: char| ch.is_ascii_digit())
                && whole.chars().all(|ch| ch.is_ascii_digit() || ch == '_')
                && parts.next().is_none_or(|fraction| fraction.chars().all(|ch| ch.is_ascii_digit()))
                && parts.next().is_none()
        }
        if completion.len() > 48 || completion.is_empty() || !numeric_atom(completion) {
            return;
        }
        crate::editor_document::edit!(self, {
            let Some(line) = self.rope.get_line(self.cursor_line) else { return; };
            let col = self.cursor_col.min(line.len_chars());
            let atom_char = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '.');
            let before = line.chars_at(col).reversed().take(97).take_while(|&ch| atom_char(ch)).count();
            let after = line.chars_at(col).take(97).take_while(|&ch| atom_char(ch)).count();
            if before > 96 || after > 96 { return; }
            let mut start = col - before;
            let end = col + after;
            if start > 0 && line.char(start - 1) == '-' {
                let preceding = line.chars_at(start - 1).reversed().take(32)
                    .find(|ch| !matches!(ch, ' ' | '\t'));
                if start == 1 || preceding.is_some_and(|ch| matches!(ch, '(' | '[' | '{' | '=' | ':' | ',' | ';')) {
                    start -= 1;
                } else if completion.starts_with('-') {
                    // Do not turn `value-1` into `value--2` or lose its operator.
                    return;
                }
            }
            let current = line.slice(start..end).to_string();
            if !numeric_atom(&current) || current == completion { return; }
            self.save_undo();
            let from = self.rope.line_to_char(self.cursor_line) + start;
            self.rope.remove(from..from + end - start);
            self.rope.insert(from, completion);
            self.cursor_col = start + completion.chars().count();
            self.modified = true;
            self.ensure_visible();
        })
    }

    /// Jump to the next occurrence of a word in the document (wraps around)
    #[cfg(test)]
    pub(crate) fn jump_to_word_occurrence(&mut self, word: &str) {
        let total = self.line_count();

        // Search from cursor position forward, wrapping around
        for offset in 1..=total {
            let line_idx = (self.cursor_line + offset) % total;
            let line_text = self.rope.line(line_idx).to_string();
            let mut pos = 0usize; // byte position
            while let Some(found) = line_text[pos..].find(word) {
                let start_byte = pos + found;
                let end_byte = start_byte + word.len();
                let at_start = start_byte == 0
                    || !line_text[..start_byte]
                        .ends_with(|c: char| c.is_alphanumeric() || c == '_');
                let at_end = end_byte >= line_text.len()
                    || !line_text[end_byte..]
                        .starts_with(|c: char| c.is_alphanumeric() || c == '_');
                if at_start && at_end {
                    let start_char = line_text[..start_byte].chars().count();
                    // Skip if it's the same position we're already at
                    if line_idx == self.cursor_line && start_char == self.cursor_col {
                        pos = start_byte + word[..].chars().next().map_or(1, char::len_utf8);
                        continue;
                    }
                    self.set_cursor(line_idx, start_char);
                    self.select_word_at_cursor();
                    return;
                }
                pos = start_byte
                    + line_text[start_byte..]
                        .chars()
                        .next()
                        .map_or(1, char::len_utf8);
            }
        }
    }

    /// Search for a needle without materializing the document. Interactive
    /// occurrence commands reuse RopeSearch directly across their matches.
    #[cfg(test)]
    pub(crate) fn find_in_rope(
        rope: &Rope,
        needle: &[char],
        start: usize,
        end: usize,
    ) -> Option<usize> {
        RopeSearch::new(&needle.iter().collect::<String>()).matches(rope, start, end).next()
    }

}

#[cfg(test)]
#[path = "editor_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "editor_cursor_tests.rs"]
mod cursor_tests;

#[cfg(test)]
#[path = "editor_core_tests.rs"]
mod core_tests;

#[cfg(test)]
#[path = "editor_interaction_performance_tests.rs"]
mod interaction_performance_tests;

#[cfg(test)]
#[path = "editor_editing_hotpath_tests.rs"]
mod editing_hotpath_tests;

#[cfg(test)]
#[path = "editor_edge_case_tests.rs"]
mod edge_case_tests;

#[cfg(test)]
#[path = "editor_input_edge_tests.rs"]
mod input_edge_tests;

#[cfg(test)]
#[path = "editor_operation_sequence_tests.rs"]
mod operation_sequence_tests;

#[cfg(test)]
#[path = "editor_history_saved_state_tests.rs"]
mod history_saved_state_tests;

#[cfg(test)]
#[path = "editor_editing_audit_tests.rs"]
mod editing_audit_tests;

#[cfg(test)]
#[path = "editor_external_save_tests.rs"]
mod external_save_tests;

#[cfg(test)]
#[path = "editor_save_performance_tests.rs"]
mod save_performance_tests;

#[cfg(test)]
#[path = "editor_io_performance_tests.rs"]
mod io_performance_tests;

#[cfg(test)]
#[path = "editor_combination_tests.rs"]
mod combination_tests;

#[cfg(test)]
#[path = "editor_correctness_audit_tests.rs"]
mod correctness_audit_tests;

#[cfg(test)]
#[path = "editor_latency_tests.rs"]
mod latency_tests;

#[cfg(test)]
#[path = "editor_shared_refresh_tests.rs"]
mod shared_refresh_tests;

#[cfg(test)]
#[path = "editor_line_command_tests.rs"]
mod line_command_tests;

#[cfg(test)]
#[path = "editor_basic_efficiency_tests.rs"]
mod basic_efficiency_tests;

#[cfg(test)]
#[path = "editor_routine_efficiency_tests.rs"]
mod routine_efficiency_tests;

#[cfg(test)]
#[path = "editor_large_file_tests.rs"]
mod large_file_tests;
