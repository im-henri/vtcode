//! A bounded, sleeping, project-local word and context index. No project walk
//! happens until explicitly requested. Queries never read files or wait on I/O.
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    hash::{Hash, Hasher},
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

#[path = "completion_examples.rs"]
pub(crate) mod examples;
#[path = "completion_storage.rs"]
mod storage;
use storage::{Catalog, StoredRecord};
#[path = "completion_index_erase.rs"]
mod erase;
#[path = "completion_index_recency.rs"]
mod recency;
#[path = "completion_index_stream.rs"]
mod stream;

const CACHE_VERSION: u32 = 2;
const MAX_WORD_BYTES: usize = 160;
const MAX_TERMS_PER_FILE: usize = 2048;
const MAX_CONTEXTS: usize = 12;
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
const MAX_PENDING_FILES: usize = 128;
const MAX_QUERY_CANDIDATES: usize = 4096;
const MAX_QUERY_RESULTS: usize = 256;
const MAX_PREFERENCE_BYTES: usize = 64 * 1024;
const MAX_CACHE_PATH_BYTES: usize = 4096;
const MAX_FILE_PAGE: usize = 32;
// Keep new learning construction and restore scratch inside the existing
// configured budget. Two bounded token builders use at most 512 KiB each;
// the remainder covers decoding and temporary occurrence keys at publication.
const LEARNING_WORKING_RESERVE: usize = 2 * 1024 * 1024;
pub(crate) const DEFAULT_MAX_FILE_BYTES: usize = 100 * 1024 * 1024;
pub(crate) const MAX_FILE_BYTES: usize = 1024 * 1024 * 1024;

fn retained_limit(settings: &IndexSettings) -> usize {
    (settings.max_memory_mb * 1024 * 1024).saturating_sub(LEARNING_WORKING_RESERVE)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum IndexSpeed {
    Low,
    #[default]
    Balanced,
    Fast,
}

impl IndexSpeed {
    fn interval(self) -> Duration {
        Duration::from_millis(match self {
            Self::Low => 150,
            Self::Balanced => 50,
            Self::Fast => 15,
        })
    }
    fn cooldown(self, elapsed: Duration) -> Duration {
        self.interval().max(elapsed.saturating_mul(match self {
            Self::Low => 24,
            Self::Balanced => 9,
            Self::Fast => 3,
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct IndexSettings {
    pub(crate) enabled: bool,
    pub(crate) project_wide: bool,
    pub(crate) paused: bool,
    pub(crate) speed: IndexSpeed,
    pub(crate) max_files: usize,
    pub(crate) max_file_bytes: usize,
    pub(crate) max_memory_mb: usize,
    pub(crate) min_prefix_chars: usize,
    pub(crate) context_triggers: bool,
    pub(crate) complete_calls: bool,
    pub(crate) use_history: bool,
}

impl Default for IndexSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            project_wide: false,
            paused: false,
            speed: IndexSpeed::Balanced,
            max_files: 20_000,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_memory_mb: 48,
            min_prefix_chars: 1,
            context_triggers: true,
            complete_calls: true,
            use_history: true,
        }
    }
}

impl IndexSettings {
    fn bounded(mut self) -> Self {
        self.max_files = self.max_files.clamp(1, 100_000);
        self.max_file_bytes = self.max_file_bytes.clamp(1024 * 1024, MAX_FILE_BYTES);
        self.max_memory_mb = self.max_memory_mb.clamp(4, 256);
        self.min_prefix_chars = self.min_prefix_chars.clamp(1, 128);
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum IndexPhase {
    Loading,
    #[default]
    Idle,
    Indexing,
    Scanning,
    Paused,
    Disabled,
    Cancelled,
    Limited,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct IndexStatus {
    pub(crate) indexed_files: usize,
    pub(crate) pending_files: usize,
    pub(crate) discovered_files: usize,
    pub(crate) skipped_files: usize,
    pub(crate) evicted_files: usize,
    pub(crate) current_file: Option<PathBuf>,
    pub(crate) recent_files: Vec<PathBuf>,
    pub(crate) phase: IndexPhase,
    pub(crate) settings: IndexSettings,
    pub(crate) memory_bytes: usize,
    pub(crate) error: Option<String>,
    pub(crate) processed_bytes: usize,
    pub(crate) current_file_bytes: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FilePage {
    pub(crate) revision: u64,
    pub(crate) total: usize,
    pub(crate) offset: usize,
    pub(crate) paths: Vec<PathBuf>,
}

/// Match a displayed file to its open-buffer aliases without accessing disk.
pub(crate) fn same_index_file_path(a: &Path, b: &Path) -> bool {
    // Remote keys preserve the server's case-sensitive identity on Windows too.
    if crate::remote::workspace::is_view_key(a) || crate::remote::workspace::is_view_key(b) {
        return a == b;
    }
    #[cfg(windows)]
    {
        a.components()
            .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
            .eq(b
                .components()
                .map(|part| part.as_os_str().to_string_lossy().to_lowercase()))
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

pub(crate) struct LearnedCandidate {
    pub(crate) word: String,
    pub(crate) context_score: i64,
    pub(crate) frequency: u32,
    pub(crate) callable: bool,
    pub(crate) domain: crate::completion_context::Domain,
    pub(crate) receiver_match: bool,
    pub(crate) history_score: i64,
    pub(crate) context_uses: u32,
    pub(crate) context_calls: u32,
}

#[derive(Clone, Serialize, Deserialize)]
// Calls are counted per context so a receiver's field can remain a value.
struct FileContext(u64, u16, u16);

type FileContexts = Vec<FileContext>;

#[derive(Clone, Serialize, Deserialize)]
struct FileTerm {
    word: Arc<str>,
    frequency: u16,
    calls: u16,
    contexts: FileContexts,
    domains: [u16; 3],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    bytes: u64,
    modified_ns: u128,
}

impl Stamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            bytes: metadata.len(),
            modified_ns: metadata
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_nanos(),
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct FileRecord {
    path: PathBuf,
    stamp: Option<Stamp>,
    hash: u64,
    terms: Arc<Vec<FileTerm>>,
    examples: Arc<examples::Data>,
    #[serde(skip)]
    dirty: bool,
    #[serde(skip)]
    memory_bytes: usize,
    #[serde(skip)]
    reducible_bytes: usize,
}

impl FileRecord {
    fn reducible_memory(&self) -> usize {
        self.reducible_bytes
    }

    fn cache_memory(&mut self) {
        self.reducible_bytes = self
            .terms
            .iter()
            .map(|term| term.contexts.capacity() * 192)
            .sum::<usize>()
            + if self.examples.tokens() > 8 {
                self.examples.tokens() * 64
            } else {
                0
            };
        self.memory_bytes = 0;
        self.memory_bytes = self.memory();
    }

    fn compact(&mut self) {
        for term in Arc::make_mut(&mut self.terms) {
            term.contexts = FileContexts::default();
        }
        Arc::make_mut(&mut self.examples).compact();
        self.cache_memory();
    }

    fn memory(&self) -> usize {
        if self.memory_bytes != 0 {
            return self.memory_bytes;
        }
        192 + self.path.as_os_str().len() * 2
            + self.examples.memory()
            + self.terms.capacity() * std::mem::size_of::<FileTerm>()
            + self
                .terms
                .iter()
                .map(|term| term.contexts.capacity() * std::mem::size_of::<FileContext>())
                .sum::<usize>()
    }
}

#[derive(Serialize, Deserialize)]
struct CacheHeader {
    version: u32,
    root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_root_uri: Option<String>,
}

fn key(word: &str) -> Arc<str> {
    let lower = word.to_lowercase();
    if lower == word {
        lower.into()
    } else {
        format!("{lower}\0{word}").into()
    }
}

fn boundaries(word: &str) -> BTreeSet<char> {
    let mut previous = '_';
    let mut result = BTreeSet::new();
    for (position, ch) in word.chars().enumerate() {
        if position == 0
            || previous == '_'
            || previous == '$'
            || (ch.is_uppercase() && !previous.is_uppercase())
        {
            result.extend(ch.to_lowercase());
        }
        previous = ch;
    }
    result
}

enum Pending {
    #[cfg(test)]
    Snapshot(ropey::Rope),
    DirtySnapshot(ropey::Rope),
    BufferSnapshot(ropey::Rope, bool),
    Disk,
    OpenedDisk,
    Release,
    Erase,
}

impl Pending {
    fn rope(&self) -> Option<&ropey::Rope> {
        match self {
            #[cfg(test)]
            Self::Snapshot(rope) => Some(rope),
            Self::DirtySnapshot(rope) | Self::BufferSnapshot(rope, _) => Some(rope),
            _ => None,
        }
    }
    fn retained_bytes(&self) -> usize {
        self.rope()
            .map_or(0, |rope| rope.len_bytes().min(MAX_PENDING_BYTES))
    }
}

#[derive(Default)]
struct Control {
    activity: crate::completion_activity::Activity,
    history_dirty: bool,
    settings: IndexSettings,
    configured: bool,
    pending: BTreeMap<PathBuf, Pending>,
    pending_bytes: usize,
    priority: Option<PathBuf>,
    touches: VecDeque<PathBuf>,
    rescan: bool,
    cancelled: bool,
    stopped: bool,
}

fn pending_erase(control: &Control, path: &Path) -> bool {
    control.pending.iter().any(|(queued, request)| {
        matches!(request, Pending::Erase) && same_index_file_path(queued, path)
    })
}

struct Shared {
    root: PathBuf,
    cache_dir: PathBuf,
    buffer_only: bool,
    control: Mutex<Control>,
    available: Condvar,
    catalog: RwLock<Catalog>,
    known: RwLock<BTreeSet<PathBuf>>,
    status: Mutex<IndexStatus>,
    wake: Mutex<crate::background::UiWake>,
    revision: AtomicU64,
    active_file: AtomicU64,
}

struct Handle(Arc<Shared>);
impl Drop for Handle {
    fn drop(&mut self) {
        let mut control = self.0.control.lock().unwrap_or_else(|e| e.into_inner());
        control.stopped = true;
        self.0.available.notify_all();
    }
}

/// Clones are cheap query handles. The last owner stops the worker; the
/// worker does not own a Handle, so it cannot keep itself alive.
#[derive(Clone)]
pub(crate) struct CompletionIndex {
    handle: Option<Arc<Handle>>,
    fallback_settings: Arc<Mutex<IndexSettings>>,
}

impl Default for CompletionIndex {
    fn default() -> Self {
        Self {
            handle: None,
            fallback_settings: Arc::new(Mutex::new(IndexSettings {
                enabled: false,
                ..IndexSettings::default()
            })),
        }
    }
}

impl CompletionIndex {
    /// Synchronous production admission for opt-in measurements. The returned
    /// guard retains file records; no scheduler delay or source I/O is timed.
    #[cfg(test)]
    pub(crate) fn build_for_benchmark<'a>(
        root: PathBuf,
        files: impl IntoIterator<Item = (PathBuf, &'a str)>,
    ) -> (Self, impl Sized) {
        let shared = Arc::new(Shared {
            cache_dir: root.join("cache"),
            buffer_only: false,
            root,
            control: Mutex::new(Control::default()),
            available: Condvar::new(),
            catalog: RwLock::new(Catalog::default()),
            known: RwLock::new(BTreeSet::new()),
            status: Mutex::new(IndexStatus::default()),
            wake: Mutex::new(crate::background::UiWake::default()),
            revision: AtomicU64::new(0),
            active_file: AtomicU64::new(0),
        });
        let mut worker = Worker::new(Arc::clone(&shared));
        let settings = IndexSettings::default();
        for (path, text) in files {
            let mut parser = stream::Extractor::default();
            parser.feed(text);
            let (terms, examples) = parser.finish_with_examples();
            worker.install(
                FileRecord {
                    path,
                    stamp: None,
                    hash: hash(text.as_bytes()),
                    terms: terms.into(),
                    examples: Arc::new(examples),
                    dirty: true,
                    memory_bytes: 0,
                    reducible_bytes: 0,
                },
                &settings,
            );
        }
        worker.publish(
            if worker.limited {
                IndexPhase::Limited
            } else {
                IndexPhase::Idle
            },
            None,
            true,
        );
        (
            Self {
                handle: Some(Arc::new(Handle(shared))),
                ..Default::default()
            },
            worker,
        )
    }

    pub(crate) fn new(root: PathBuf) -> Self {
        let directory = crate::settings::project_settings_path(&root)
            .parent()
            .unwrap()
            .to_owned();
        Self::new_in(root, directory)
    }

    /// Remote sources come from loaded buffers; their bounded learned cache is
    /// stored locally under a connection/workspace identity, never a view key.
    pub(crate) fn new_remote(workspace: &crate::remote::workspace::Workspace) -> Self {
        let root = workspace.view_root.clone();
        let identity = crate::portable::workspace_dir(&root);
        let directory = crate::portable::data_dir()
            .join("remote")
            .join(identity.file_name().unwrap())
            // Retain the cache location across the VTComplete rename.
            .join("autocomplete");
        Self::new_with_identity(
            root,
            directory,
            IndexSettings::default(),
            true,
            Some(workspace.client.hello().root_uri.clone()),
        )
    }

    pub(crate) fn buffer_only(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| handle.0.buffer_only)
    }

    fn new_in(root: PathBuf, cache_dir: PathBuf) -> Self {
        Self::new_with_defaults(root, cache_dir, IndexSettings::default())
    }

    fn new_with_defaults(root: PathBuf, cache_dir: PathBuf, settings: IndexSettings) -> Self {
        Self::new_with_mode(root, cache_dir, settings, false)
    }

    fn new_with_mode(
        root: PathBuf,
        cache_dir: PathBuf,
        settings: IndexSettings,
        buffer_only: bool,
    ) -> Self {
        Self::new_with_identity(root, cache_dir, settings, buffer_only, None)
    }

    fn new_with_identity(
        root: PathBuf,
        cache_dir: PathBuf,
        mut settings: IndexSettings,
        buffer_only: bool,
        remote_root_uri: Option<String>,
    ) -> Self {
        if buffer_only {
            settings.project_wide = false;
        }
        let shared = Arc::new(Shared {
            root,
            cache_dir,
            buffer_only,
            control: Mutex::new(Control {
                settings,
                ..Control::default()
            }),
            available: Condvar::new(),
            catalog: RwLock::new(Catalog::default()),
            known: RwLock::new(BTreeSet::new()),
            status: Mutex::new(IndexStatus {
                phase: IndexPhase::Loading,
                ..IndexStatus::default()
            }),
            wake: Mutex::new(crate::background::UiWake::default()),
            revision: AtomicU64::new(0),
            active_file: AtomicU64::new(0),
        });
        let worker = Arc::clone(&shared);
        if let Err(error) = std::thread::Builder::new()
            .name("completion-index".into())
            .spawn(move || {
                let mut worker = Worker::new(worker);
                worker.remote_root_uri = remote_root_uri;
                worker.run();
            })
        {
            let mut status = shared.status.lock().unwrap_or_else(|e| e.into_inner());
            status.phase = IndexPhase::Disabled;
            status.error = Some(error.to_string());
        }
        Self {
            handle: Some(Arc::new(Handle(shared))),
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(root: PathBuf, cache_dir: PathBuf) -> Self {
        // Existing indexing fixtures explicitly exercise a running worker.
        // Persisted preferences still override these unconfigured defaults.
        Self::new_with_defaults(
            root,
            cache_dir,
            IndexSettings {
                paused: false,
                ..IndexSettings::default()
            },
        )
    }

    pub(crate) fn set_wake(&self, wake: crate::background::UiWake) {
        if let Some(handle) = &self.handle {
            *handle.0.wake.lock().unwrap_or_else(|e| e.into_inner()) = wake;
        }
    }

    pub(crate) fn set_activity(&self, activity: crate::completion_activity::Activity) {
        if let Some(handle) = &self.handle {
            handle
                .0
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .activity = activity;
        }
    }

    pub(crate) fn revision(&self) -> u64 {
        self.handle
            .as_ref()
            .map_or(0, |h| h.0.revision.load(Ordering::Acquire))
    }

    pub(crate) fn status(&self) -> IndexStatus {
        self.handle.as_ref().map_or_else(
            || IndexStatus {
                phase: IndexPhase::Disabled,
                settings: IndexSettings {
                    enabled: false,
                    ..IndexSettings::default()
                },
                ..IndexStatus::default()
            },
            |h| h.0.status.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        )
    }

    /// Read one bounded page from published in-memory paths. No source files
    /// are inspected, and the complete catalog is never cloned into the UI.
    pub(crate) fn file_page(&self, offset: usize, limit: usize) -> FilePage {
        let Some(handle) = &self.handle else {
            return FilePage::default();
        };
        let shared = &handle.0;
        let known = shared
            .known
            .read()
            .unwrap_or_else(|error| error.into_inner());
        let total = known.len();
        let limit = limit.min(MAX_FILE_PAGE);
        let offset = offset.min(total.saturating_sub(1) / limit.max(1) * limit.max(1));
        FilePage {
            revision: shared.revision.load(Ordering::Acquire),
            total,
            offset,
            paths: known.iter().skip(offset).take(limit).cloned().collect(),
        }
    }

    /// Queue a one-time erase of the file's memory and checkpoint contributions.
    /// No exclusion rule is retained: a later observation may index it again.
    pub(crate) fn remove_file(&self, path: &Path) -> bool {
        let Some(handle) = &self.handle else {
            return false;
        };
        let shared = &handle.0;
        if !Self::eligible(shared, path) || path.as_os_str().len() > MAX_CACHE_PATH_BYTES {
            return false;
        }
        let mut control = shared
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if control.stopped {
            return false;
        }
        if pending_erase(&control, path) {
            return true;
        }
        let existing = control
            .pending
            .keys()
            .any(|key| same_index_file_path(key, path));
        if control.pending.len() >= MAX_PENDING_FILES && !existing {
            return false;
        }
        control
            .pending
            .retain(|key, _| !same_index_file_path(key, path));
        control.pending_bytes = control.pending.values().map(Pending::retained_bytes).sum();
        control.pending.insert(path.to_owned(), Pending::Erase);
        drop(control);
        shared.revision.fetch_add(1, Ordering::Release);
        shared.available.notify_all();
        shared
            .wake
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .notify();
        true
    }

    pub(crate) fn settings(&self) -> IndexSettings {
        self.handle.as_ref().map_or_else(
            || {
                self.fallback_settings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
            },
            |h| {
                h.0.control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .settings
                    .clone()
            },
        )
    }

    pub(crate) fn configure(&self, settings: IndexSettings) {
        let Some(handle) = &self.handle else {
            *self
                .fallback_settings
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = settings.bounded();
            return;
        };
        let mut control = handle.0.control.lock().unwrap_or_else(|e| e.into_inner());
        let mut settings = settings.bounded();
        if handle.0.buffer_only {
            settings.project_wide = false;
        }
        if settings == control.settings && control.configured {
            return;
        }
        let start_scan = settings.enabled
            && settings.project_wide
            && (!control.settings.enabled || !control.settings.project_wide);
        let larger_limits = settings.enabled
            && (settings.max_files > control.settings.max_files
                || settings.max_file_bytes > control.settings.max_file_bytes
                || settings.max_memory_mb > control.settings.max_memory_mb);
        control.settings = settings;
        control.configured = true;
        control.cancelled = false;
        control.rescan |= start_scan || larger_limits;
        if !control.settings.enabled {
            control
                .pending
                .retain(|_, request| matches!(request, Pending::Erase));
            control.pending_bytes = 0;
        }
        handle.0.available.notify_all();
    }

    fn eligible(shared: &Shared, path: &Path) -> bool {
        path.strip_prefix(&shared.root).is_ok_and(|relative| {
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
        })
    }

    #[cfg(test)]
    pub(crate) fn observe(&self, path: PathBuf, rope: ropey::Rope) -> bool {
        self.observe_source(path, Pending::Snapshot(rope))
    }

    pub(crate) fn observe_buffer(&self, path: PathBuf, rope: ropey::Rope, modified: bool) -> bool {
        self.observe_source(
            path,
            if self.buffer_only() {
                Pending::BufferSnapshot(rope, modified)
            } else if modified {
                Pending::DirtySnapshot(rope)
            } else {
                Pending::OpenedDisk
            },
        )
    }

    /// Prioritize the current editor without reading disk or scanning the index.
    /// Accepted updates still use the bounded, coalescing worker queue.
    pub(crate) fn observe_active_buffer(
        &self,
        path: PathBuf,
        rope: ropey::Rope,
        modified: bool,
    ) -> bool {
        self.queue_source(
            path,
            if self.buffer_only() {
                Pending::BufferSnapshot(rope, modified)
            } else if modified {
                Pending::DirtySnapshot(rope)
            } else {
                Pending::OpenedDisk
            },
            true,
        )
    }

    pub(crate) fn clear_active_file(&self) {
        if let Some(handle) = &self.handle {
            handle.0.active_file.store(0, Ordering::Release);
            handle
                .0
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .priority = None;
        }
    }

    fn observe_source(&self, path: PathBuf, source: Pending) -> bool {
        self.queue_source(path, source, false)
    }

    fn queue_source(&self, path: PathBuf, source: Pending, active: bool) -> bool {
        let Some(handle) = &self.handle else {
            return true;
        };
        let shared = &handle.0;
        if !Self::eligible(shared, &path) {
            return true;
        }
        if active {
            shared.active_file.store(
                crate::completion_source::Source::new(Some(&path), "").file,
                Ordering::Release,
            );
        }
        let mut control = shared.control.lock().unwrap_or_else(|e| e.into_inner());
        if !control.settings.enabled
            || pending_erase(&control, &path)
            || source
                .rope()
                .is_some_and(|rope| rope.len_bytes() > control.settings.max_file_bytes)
        {
            return true;
        }
        if active {
            if control.priority.as_ref() != Some(&path) {
                // Preserve rapid A -> B -> C focus changes even if the worker
                // coalesces their scans. The previous file was used until now.
                if let Some(previous) = control.priority.clone() {
                    control.touches.push_back(previous);
                }
                control.touches.push_back(path.clone());
                while control.touches.len() > MAX_PENDING_FILES {
                    control.touches.pop_front();
                }
            }
            control.priority = Some(path.clone());
        }
        if control
            .pending
            .get(&path)
            .and_then(Pending::rope)
            .zip(source.rope())
            .is_some_and(|(previous, rope)| previous.is_instance(rope))
        {
            return true;
        }
        let old_bytes = control
            .pending
            .get(&path)
            .map_or(0, Pending::retained_bytes);
        let bytes = source.retained_bytes();
        if (control.pending.len() >= MAX_PENDING_FILES && !control.pending.contains_key(&path))
            || control.pending_bytes.saturating_sub(old_bytes) + bytes > MAX_PENDING_BYTES
        {
            return false;
        }
        control.pending_bytes = control.pending_bytes.saturating_sub(old_bytes) + bytes;
        control.pending.insert(path, source);
        control.cancelled = false;
        drop(control);
        shared.available.notify_all();
        true
    }

    /// Forget an unsaved overlay when its last editor closes. The saved file
    /// is revalidated on the worker; discarded buffer words disappear.
    pub(crate) fn release(&self, path: PathBuf) {
        let Some(handle) = &self.handle else {
            return;
        };
        if !Self::eligible(&handle.0, &path) {
            return;
        }
        let mut control = handle.0.control.lock().unwrap_or_else(|e| e.into_inner());
        if pending_erase(&control, &path) {
            return;
        }
        // A clean buffer queued just before closing still belongs in the
        // session's learned files. There is no remote disk read to replace it.
        if handle.0.buffer_only
            && matches!(
                control.pending.get(&path),
                Some(Pending::BufferSnapshot(_, false))
            )
        {
            return;
        }
        if let Some(previous) = control.pending.remove(&path) {
            control.pending_bytes = control
                .pending_bytes
                .saturating_sub(previous.retained_bytes());
        }
        // Releases are lifecycle events and must not be lost to queue pressure.
        control.pending.insert(path, Pending::Release);
        handle.0.available.notify_all();
    }

    /// Filesystem notifications never cause a walk in opened-files mode.
    pub(crate) fn changed(&self, path: PathBuf) {
        let Some(handle) = &self.handle else {
            return;
        };
        if handle.0.buffer_only || !Self::eligible(&handle.0, &path) {
            return;
        }
        let mut control = handle.0.control.lock().unwrap_or_else(|e| e.into_inner());
        if !control.settings.enabled || pending_erase(&control, &path) {
            return;
        }
        if !control.settings.project_wide
            && !handle
                .0
                .known
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .range(path.clone()..)
                .next()
                .is_some_and(|known| known.starts_with(&path))
        {
            return;
        }
        if control.pending.len() >= MAX_PENDING_FILES {
            control.rescan = true;
            handle.0.available.notify_all();
            return;
        }
        // An unsaved open buffer always wins over a disk notification.
        control.pending.entry(path).or_insert(Pending::Disk);
        handle.0.available.notify_all();
    }

    pub(crate) fn cancel(&self) {
        let Some(handle) = &self.handle else {
            return;
        };
        let mut control = handle.0.control.lock().unwrap_or_else(|e| e.into_inner());
        control
            .pending
            .retain(|_, request| matches!(request, Pending::Erase));
        control.pending_bytes = 0;
        control.rescan = false;
        control.cancelled = true;
        handle.0.available.notify_all();
    }

    pub(crate) fn rescan(&self) {
        let Some(handle) = &self.handle else {
            return;
        };
        let mut control = handle.0.control.lock().unwrap_or_else(|e| e.into_inner());
        control.rescan = true;
        control.cancelled = false;
        handle.0.available.notify_all();
    }

    #[cfg(test)]
    pub(crate) fn query(&self, prefix: &str, context: &str) -> Vec<LearnedCandidate> {
        let mut analyzer = crate::completion_context::Analyzer::default();
        analyzer.feed(context, |_| {});
        analyzer.flush(|_| {});
        let structural = analyzer.context();
        let history = self.history_scores(prefix, structural, true);
        self.query_context(prefix, context, structural, &history)
    }

    #[cfg(test)]
    pub(crate) fn query_context(
        &self,
        prefix: &str,
        context: &str,
        structural: crate::completion_context::Context,
        history: &BTreeMap<String, i64>,
    ) -> Vec<LearnedCandidate> {
        self.query_for_invocation(prefix, context, structural, history, true)
    }

    /// Bounded occurrence retrieval from already-published memory. Publication
    /// never blocks completion and no source file is opened by this path.
    pub(crate) fn learn_examples(
        &self,
        query: &examples::Query,
        active_file: u64,
        evidence: &mut examples::Evidence,
    ) {
        let Some(handle) = &self.handle else {
            return;
        };
        if !self.settings().enabled {
            return;
        }
        let Ok(catalog) = handle.0.catalog.try_read() else {
            return;
        };
        catalog.examples.query(query, active_file, evidence);
    }

    /// The actual current buffer's vocabulary is a cache level, independent of
    /// extension or file relationships. Other files contribute via occurrences.
    pub(crate) fn current_vocabulary(&self, file: u64, prefix: &str) -> Vec<(String, u32, u32)> {
        let Some(handle) = &self.handle else {
            return Vec::new();
        };
        if !self.settings().enabled {
            return Vec::new();
        }
        let Ok(catalog) = handle.0.catalog.try_read() else {
            return Vec::new();
        };
        catalog.vocabulary(file, prefix)
    }

    #[cfg(test)]
    pub(crate) fn example_memory(&self) -> usize {
        self.handle.as_ref().map_or(0, |handle| {
            handle
                .0
                .catalog
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .examples
                .retained_memory()
        })
    }

    #[cfg(test)]
    pub(crate) fn query_for_invocation(
        &self,
        prefix: &str,
        context: &str,
        structural: crate::completion_context::Context,
        history: &BTreeMap<String, i64>,
        explicit: bool,
    ) -> Vec<LearnedCandidate> {
        self.query_for_invocation_cancellable(
            prefix,
            context,
            structural,
            history,
            explicit,
            &|| false,
        )
    }

    pub(crate) fn query_for_invocation_cancellable(
        &self,
        prefix: &str,
        context: &str,
        structural: crate::completion_context::Context,
        history: &BTreeMap<String, i64>,
        explicit: bool,
        cancelled: &impl Fn() -> bool,
    ) -> Vec<LearnedCandidate> {
        let Some(handle) = &self.handle else {
            return Vec::new();
        };
        if prefix.len() > MAX_WORD_BYTES || cancelled() {
            return Vec::new();
        }
        let settings = self.settings();
        if !settings.enabled {
            return Vec::new();
        }
        // A query never waits behind publication. A newer request will see it.
        let Ok(catalog) = handle.0.catalog.try_read() else {
            return Vec::new();
        };
        catalog.query_cancellable(prefix, context, structural, history, explicit, cancelled)
    }

    #[cfg(test)]
    pub(crate) fn record_acceptance(&self, word: &str, context: &str) {
        let mut analyzer = crate::completion_context::Analyzer::default();
        analyzer.feed(context, |_| {});
        analyzer.flush(|_| {});
        self.record_choice(word, "", analyzer.context(), true);
    }

    pub(crate) fn history_scores(
        &self,
        prefix: &str,
        context: crate::completion_context::Context,
        explicit: bool,
    ) -> BTreeMap<String, i64> {
        if !self.settings().use_history {
            return BTreeMap::new();
        }
        self.handle
            .as_ref()
            .and_then(|handle| handle.0.catalog.try_read().ok())
            .map_or_else(BTreeMap::new, |catalog| {
                catalog.history.scores(prefix, context, explicit)
            })
    }

    pub(crate) fn record_choice(
        &self,
        word: &str,
        prefix: &str,
        context: crate::completion_context::Context,
        manual: bool,
    ) {
        let Some(handle) = &self.handle else {
            return;
        };
        if !self.settings().use_history {
            return;
        }
        if word.len() > MAX_WORD_BYTES {
            return;
        }
        let Ok(mut catalog) = handle.0.catalog.try_write() else {
            return;
        };
        catalog.history.record(word, prefix, context, manual);
        drop(catalog);
        let mut control = handle
            .0
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        control.history_dirty = true;
        handle.0.available.notify_all();
    }
}

fn subsequence(query: &str, word: &str) -> bool {
    let mut chars = word.chars();
    query
        .chars()
        .all(|wanted| chars.by_ref().any(|ch| ch == wanted))
}

fn hash(value: impl Hash) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// Capture the literal receiver and punctuation, plus the preceding two words.
/// The same extraction is used for learning and queries, for every language.
fn context_keys(text: &str) -> Vec<u64> {
    let start = text
        .char_indices()
        .rev()
        .nth(255)
        .map_or(0, |(byte, _)| byte);
    let text = &text[start..];
    let mut words = VecDeque::new();
    let mut word_start = None;
    let mut last_end = 0;
    for (byte, ch) in text.char_indices() {
        if is_word(ch) {
            word_start.get_or_insert(byte);
        } else if let Some(start) = word_start.take() {
            if words.len() == 2 {
                words.pop_front();
            }
            words.push_back(&text[start..byte]);
            last_end = byte;
        }
    }
    if let Some(start) = word_start {
        if words.len() == 2 {
            words.pop_front();
        }
        words.push_back(&text[start..]);
        last_end = text.len();
    }
    let punctuation: String = text[last_end..]
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .take(8)
        .collect();
    observation_keys(
        &words.into_iter().map(str::to_owned).collect(),
        &punctuation,
    )
    .into_iter()
    .flatten()
    .collect()
}

fn observation_keys(words: &VecDeque<String>, punctuation: &str) -> [Option<u64>; 3] {
    let mut result = [None; 3];
    if let Some(previous) = words.back() {
        result[0] = Some(hash((previous, punctuation)));
        if words.len() == 2 {
            result[1] = Some(hash((&words[0], previous, punctuation)));
        }
    }
    if let Some(shape) = ["->", "::", "&", "*", ".", "=", "(", ",", ":"]
        .into_iter()
        .find(|shape| punctuation.ends_with(shape))
    {
        result[2] = Some(hash(("punctuation", shape)));
    }
    result
}

#[cfg(test)]
fn extract(text: &str) -> Vec<FileTerm> {
    let mut extractor = stream::Extractor::default();
    extractor.feed(text);
    extractor.finish()
}
#[derive(Default)]
struct WatchRules {
    files: BTreeMap<PathBuf, (Stamp, ignore::gitignore::Gitignore)>,
    global: Option<ignore::gitignore::Gitignore>,
}

impl WatchRules {
    fn matched_file(
        &mut self,
        directory: &Path,
        file: &Path,
        candidate: &Path,
        is_dir: bool,
    ) -> Option<bool> {
        let Some(stamp) = Stamp::read(file) else {
            self.files.remove(file);
            return None;
        };
        if !self
            .files
            .get(file)
            .is_some_and(|(previous, _)| *previous == stamp)
        {
            // Cache only existing rule files; missing ancestor rule files do
            // not grow the cache. Bound compilation and retained rule sets.
            let Ok(bytes) = read_bounded(file, 64 * 1024) else {
                return Some(true);
            };
            let mut builder = ignore::gitignore::GitignoreBuilder::new(directory);
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<_> = text.lines().take(513).collect();
            if lines.len() > 512 {
                return Some(true);
            }
            for line in lines {
                let _ = builder.add_line(Some(file.to_owned()), line);
            }
            let Ok(matcher) = builder.build() else {
                return Some(true);
            };
            if self.files.len() >= 16
                && let Some(first) = self.files.keys().next().cloned()
            {
                self.files.remove(&first);
            }
            self.files.insert(file.to_owned(), (stamp, matcher));
        }
        let matched = self.files.get(file)?.1.matched(candidate, is_dir);
        if matched.is_none() {
            None
        } else {
            Some(matched.is_ignore())
        }
    }

    fn permits(&mut self, root: &Path, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let ancestors: Vec<_> = parent.ancestors().take(128).collect();
        if self.global.is_none() {
            self.global = Some(
                ignore::gitignore::GitignoreBuilder::new(root)
                    .build_global()
                    .0,
            );
        }
        // Test directories before their children, matching walk pruning.
        // A child whitelist cannot reopen an ignored parent directory.
        let mut entries: Vec<_> = path
            .ancestors()
            .take_while(|entry| *entry != root)
            .take(129)
            .collect();
        if entries.len() > 128 {
            return false;
        }
        entries.reverse();
        for entry in entries {
            let is_dir = entry != path;
            if is_dir && excluded_directory(entry.file_name().unwrap_or_default())
                || std::fs::symlink_metadata(entry)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                return false;
            }
            let applicable = || {
                ancestors
                    .iter()
                    .copied()
                    .filter(|directory| entry != *directory && entry.starts_with(directory))
            };
            let mut ignored = None;
            for directory in applicable() {
                if let Some(result) =
                    self.matched_file(directory, &directory.join(".ignore"), entry, is_dir)
                {
                    ignored = Some(result);
                    break;
                }
            }
            if ignored.is_none() {
                for directory in applicable() {
                    if let Some(result) =
                        self.matched_file(directory, &directory.join(".gitignore"), entry, is_dir)
                    {
                        ignored = Some(result);
                        break;
                    }
                    if directory.join(".git").exists() {
                        break;
                    }
                }
            }
            if ignored.is_none() {
                for directory in applicable() {
                    if directory.join(".git").exists() {
                        ignored = self.matched_file(
                            directory,
                            &directory.join(".git/info/exclude"),
                            entry,
                            is_dir,
                        );
                        break;
                    }
                }
            }
            if ignored.is_none()
                && let Some(global) = &self.global
            {
                let matched = global.matched(entry, is_dir);
                if !matched.is_none() {
                    ignored = Some(matched.is_ignore());
                }
            }
            if ignored == Some(true)
                || (ignored.is_none()
                    && entry
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with('.')))
            {
                return false;
            }
        }
        true
    }
}

fn excluded_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            ".git"
                | ".hg"
                | ".svn"
                | "node_modules"
                | "target"
                | "dist"
                | "build"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".vtcode"
        )
    )
}

struct Worker {
    history_dirty: bool,
    remote_root_uri: Option<String>,
    shared: Arc<Shared>,
    files: BTreeMap<PathBuf, Arc<StoredRecord>>,
    validation: VecDeque<PathBuf>,
    restore: Option<std::io::BufReader<std::fs::File>>,
    walk: Option<ignore::Walk>,
    memory: usize,
    skipped: usize,
    discovered: usize,
    recent: VecDeque<PathBuf>,
    limited: bool,
    cache_dirty: bool,
    checkpoint_complete: bool,
    preferences_dirty: bool,
    last_save: Instant,
    last_publish: Instant,
    last_settings: IndexSettings,
    watch_rules: WatchRules,
    scans: VecDeque<stream::Scan>,
    // One bounded path in the existing working reserve. A clock avoids
    // rescanning every term in every file for each cache-reclamation step.
    compact_after: PathBuf,
    recency: recency::Recency,
    admission: Option<FileRecord>,
    active_path: Option<PathBuf>,
    example_bytes: usize,
    evicted: usize,
    oversized_after: Option<PathBuf>,
}

impl Worker {
    fn new(shared: Arc<Shared>) -> Self {
        Self {
            history_dirty: false,
            remote_root_uri: None,
            shared,
            files: BTreeMap::new(),
            validation: VecDeque::new(),
            restore: None,
            walk: None,
            memory: 0,
            skipped: 0,
            discovered: 0,
            recent: VecDeque::new(),
            limited: false,
            cache_dirty: false,
            checkpoint_complete: true,
            preferences_dirty: false,
            last_save: Instant::now(),
            last_publish: Instant::now() - Duration::from_secs(1),
            last_settings: IndexSettings::default(),
            watch_rules: WatchRules::default(),
            scans: VecDeque::new(),
            compact_after: PathBuf::new(),
            recency: recency::Recency::default(),
            admission: None,
            active_path: None,
            example_bytes: 0,
            evicted: 0,
            oversized_after: None,
        }
    }

    fn run(mut self) {
        crate::worker_priority::lower_current_thread();
        let _priority = crate::worker_priority::CompletionPriority::new();
        self.restore();
        loop {
            let delay = {
                let control = self
                    .shared
                    .control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                (!control.stopped
                    && !control.cancelled
                    && control.settings.enabled
                    && (!control.settings.paused
                        || self.shared.buffer_only && self.restore.is_some()))
                .then(|| control.activity.remaining())
                .flatten()
            };
            if let Some(delay) = delay {
                if !self.wait(Some(delay.min(Duration::from_millis(10)))) {
                    self.save();
                    return;
                }
                continue;
            }
            self.erase_pending();
            let (settings, pending, priority, touches, rescan, cancelled, stopped) = {
                let mut control = self
                    .shared
                    .control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let settings = control.settings.clone();
                self.history_dirty |= std::mem::take(&mut control.history_dirty);
                let replacement = self
                    .scans
                    .iter()
                    .find_map(|scan| {
                        control
                            .pending
                            .get(&scan.path)
                            .is_some_and(|request| !matches!(request, Pending::Erase))
                            .then(|| scan.path.clone())
                    })
                    .or_else(|| {
                        self.admission.as_ref().and_then(|record| {
                            control
                                .pending
                                .contains_key(&record.path)
                                .then(|| record.path.clone())
                        })
                    });
                let priority = control
                    .priority
                    .clone()
                    .filter(|path| control.pending.contains_key(path));
                let pending = if settings.enabled
                    && !settings.paused
                    && !control.cancelled
                    && (self.admission.is_none()
                        || priority.is_some() && !self.shared.buffer_only
                        || replacement.is_some())
                    && (self.scans.len() < 2
                        || replacement.is_some()
                        || priority.is_some() && !self.shared.buffer_only)
                {
                    priority
                        .clone()
                        .or(replacement)
                        .and_then(|path| control.pending.remove_entry(&path))
                        .or_else(|| {
                            let path = control
                                .pending
                                .iter()
                                .find(|(_, request)| !matches!(request, Pending::Erase))
                                .map(|(path, _)| path.clone())?;
                            control.pending.remove_entry(&path)
                        })
                        .map(|(path, pending)| {
                            control.pending_bytes = control
                                .pending_bytes
                                .saturating_sub(pending.retained_bytes());
                            (path, pending)
                        })
                } else {
                    None
                };
                (
                    settings,
                    pending,
                    priority,
                    std::mem::take(&mut control.touches),
                    std::mem::take(&mut control.rescan),
                    control.cancelled,
                    control.stopped,
                )
            };
            for path in touches {
                self.touch_path(&path);
            }
            if settings != self.last_settings {
                let reenable = settings.enabled && !self.last_settings.enabled;
                if settings.max_file_bytes < self.last_settings.max_file_bytes {
                    self.oversized_after = Some(PathBuf::new());
                }
                self.last_settings = settings.clone();
                self.preferences_dirty = true;
                if !settings.enabled {
                    self.restore = None;
                    self.scans.clear();
                    self.admission = None;
                }
                self.scans
                    .retain(|scan| scan.progress().1 <= settings.max_file_bytes);
                if self.admission.as_ref().is_some_and(|record| {
                    record
                        .stamp
                        .as_ref()
                        .is_some_and(|stamp| stamp.bytes > settings.max_file_bytes as u64)
                }) {
                    self.admission = None;
                }
                if !settings.enabled || !settings.project_wide {
                    self.walk = None;
                }
                if reenable {
                    self.restore_cache(&settings);
                }
                self.enforce_limits(&settings);
            }
            if cancelled {
                self.walk = None;
                self.validation.clear();
                self.restore = None;
                self.scans.clear();
                self.admission = None;
            }
            if stopped {
                self.save();
                return;
            }
            if rescan && settings.enabled && !self.shared.buffer_only {
                self.limited = false;
                if !self.checkpoint_complete {
                    self.restore_cache(&settings);
                }
                self.validation = self
                    .files
                    .values()
                    .filter(|record| !record.dirty)
                    .map(|record| record.path.clone())
                    .collect();
                if settings.project_wide {
                    self.begin_walk();
                }
            }
            if self.preferences_dirty {
                self.save_preferences(&settings);
            }
            // Pausing stops source learning, not use of a matching saved cache.
            // Restore in the same bounded slices without consuming buffer jobs.
            if self.shared.buffer_only
                && settings.enabled
                && settings.paused
                && self.scans.is_empty()
                && (self.restore.is_some() || self.admission.is_some())
            {
                let started = Instant::now();
                if self.admission.is_some() {
                    self.advance_admission(&settings);
                } else {
                    for _ in 0..16 {
                        if !self.validate_next(&settings)
                            || self.admission.is_some()
                            || started.elapsed() >= Duration::from_millis(3)
                        {
                            break;
                        }
                    }
                }
                self.publish(IndexPhase::Paused, None, false);
                if !self.wait(Some(settings.speed.cooldown(started.elapsed()))) {
                    self.save();
                    return;
                }
                continue;
            }
            if !settings.enabled || settings.paused || cancelled {
                let phase = if !settings.enabled {
                    IndexPhase::Disabled
                } else if settings.paused {
                    IndexPhase::Paused
                } else {
                    IndexPhase::Cancelled
                };
                self.publish(phase, None, true);
                self.save();
                if self.wait(self.pending_save_wait()) {
                    continue;
                }
                self.save();
                return;
            }
            let started = Instant::now();
            let worked = if let Some((path, pending)) = pending {
                if priority.as_ref() == Some(&path) {
                    self.active_path = Some(path.clone());
                    self.touch_path(&path);
                    // An active file must not wait for two unrelated large scans.
                    // Discarded cache work is refreshed on the next activation.
                    if !self.shared.buffer_only {
                        self.scans.retain(|scan| scan.path == path);
                        self.admission = None;
                    }
                }
                let was_scanning = self
                    .scans
                    .iter()
                    .any(|scan| same_index_file_path(&scan.path, &path))
                    || self
                        .admission
                        .as_ref()
                        .is_some_and(|record| same_index_file_path(&record.path, &path));
                let retain_clean = self.shared.buffer_only && matches!(pending, Pending::Release);
                if self.admission.as_ref().is_some_and(|record| {
                    same_index_file_path(&record.path, &path) && (!retain_clean || record.dirty)
                }) {
                    self.admission = None;
                }
                self.scans.retain(|scan| {
                    !same_index_file_path(&scan.path, &path) || retain_clean && scan.clean_buffer()
                });
                self.publish(IndexPhase::Indexing, Some(path.clone()), false);
                match pending {
                    #[cfg(test)]
                    Pending::Snapshot(rope) => self.index_snapshot(path, rope, &settings),
                    Pending::DirtySnapshot(rope) => {
                        self.queue_snapshot(path, rope, &settings, true)
                    }
                    Pending::BufferSnapshot(rope, modified) => {
                        self.scans.push_front(stream::Scan::buffer(
                            path,
                            rope,
                            settings.max_file_bytes,
                            modified,
                        ));
                    }
                    Pending::OpenedDisk => {
                        if self.files.get(&path).is_some_and(|record| record.dirty) {
                            self.remove(&path);
                        }
                        self.index_opened_disk(path, &settings);
                    }
                    Pending::Disk => {
                        if self.files.contains_key(&path)
                            || (settings.project_wide
                                && self.watch_rules.permits(&self.shared.root, &path))
                        {
                            self.index_disk(path.clone(), &settings);
                        }
                        if !self.files.contains_key(&path) {
                            self.validation.extend(
                                self.files
                                    .keys()
                                    .filter(|known| known.starts_with(&path))
                                    .cloned(),
                            );
                        }
                    }
                    Pending::Erase => unreachable!("erase commands are processed separately"),
                    Pending::Release if self.files.contains_key(&path) || was_scanning => {
                        if self.files.get(&path).is_some_and(|record| record.dirty) {
                            self.remove(&path);
                        }
                        self.index_disk(path, &settings);
                    }
                    Pending::Release => {}
                }
                if !self.scans.is_empty() {
                    self.advance_scan(&settings);
                }
                true
            } else if self.admission.is_some() {
                self.advance_admission(&settings);
                true
            } else if self.oversized_after.is_some() {
                self.trim_oversized(&settings);
                true
            } else if !self.scans.is_empty() {
                self.advance_scan(&settings);
                true
            } else if !self.validation.is_empty() || self.restore.is_some() {
                // Metadata and compact cached records are much cheaper than
                // reading/tokenizing files. Warm them in short batches while
                // preserving the same measured CPU duty cycle.
                for _ in 0..16 {
                    if !self.validate_next(&settings)
                        || !self.scans.is_empty()
                        || started.elapsed() >= Duration::from_millis(3)
                    {
                        break;
                    }
                }
                if !self.scans.is_empty() {
                    self.advance_scan(&settings);
                }
                true
            } else if self.walk.is_some() {
                self.scan_step(&settings);
                if !self.scans.is_empty() {
                    self.advance_scan(&settings);
                }
                true
            } else {
                false
            };
            if self.cache_dirty && self.last_save.elapsed() > Duration::from_secs(3) {
                self.save();
            }
            if worked {
                self.publish(
                    if self.walk.is_some() {
                        IndexPhase::Scanning
                    } else {
                        IndexPhase::Indexing
                    },
                    None,
                    false,
                );
                // One bounded file or 32 directory entries per turn. Control
                // changes interrupt the throttle immediately.
                let deadline = Instant::now() + settings.speed.cooldown(started.elapsed());
                while Instant::now() < deadline {
                    if !self.wait(Some(deadline.saturating_duration_since(Instant::now()))) {
                        self.save();
                        return;
                    }
                    let control = self
                        .shared
                        .control
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if control.settings != settings
                        || control.cancelled
                        || control.rescan
                        || control
                            .pending
                            .values()
                            .any(|request| matches!(request, Pending::Erase))
                        || control.priority.as_ref().is_some_and(|path| {
                            control.pending.contains_key(path)
                                && self.active_path.as_ref() != Some(path)
                        })
                    {
                        break;
                    }
                }
            } else {
                self.save();
                self.publish(
                    if self.limited {
                        IndexPhase::Limited
                    } else {
                        IndexPhase::Idle
                    },
                    None,
                    true,
                );
                if !self.wait(self.pending_save_wait()) {
                    self.save();
                    return;
                }
            }
        }
    }

    fn wait(&self, delay: Option<Duration>) -> bool {
        let control = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if control.stopped {
            return false;
        }
        if delay.is_none()
            && (!control.pending.is_empty()
                && control.settings.enabled
                && !control.settings.paused
                && !control.cancelled
                || control.rescan
                || control.history_dirty
                || control
                    .pending
                    .values()
                    .any(|request| matches!(request, Pending::Erase))
                || control.settings != self.last_settings)
        {
            return true;
        }
        let control = if let Some(delay) = delay {
            self.shared
                .available
                .wait_timeout(control, delay)
                .unwrap_or_else(|e| e.into_inner())
                .0
        } else {
            self.shared
                .available
                .wait(control)
                .unwrap_or_else(|e| e.into_inner())
        };
        !control.stopped
    }

    fn restore(&mut self) {
        // Discard obsolete exclusion artifacts; no rules are read or migrated.
        for name in [
            "completion-removed-files.json",
            "completion-removed-files.backup.json",
        ]
        .into_iter()
        .filter(|_| !self.shared.buffer_only)
        {
            if let Err(error) = std::fs::remove_file(self.shared.cache_dir.join(name))
                && error.kind() != std::io::ErrorKind::NotFound
            {
                self.error(error);
            }
        }
        if !self.shared.buffer_only
            && let Ok(bytes) = read_bounded(
                &self.shared.cache_dir.join("completion-history.json"),
                256 * 1024,
            )
            && let Ok(history) =
                serde_json::from_slice::<crate::completion_history::History>(&bytes)
        {
            self.shared
                .catalog
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .history = history.bounded();
        }
        let settings_path = self.shared.cache_dir.join("completion-settings.json");
        let mut settings_paths = vec![
            settings_path.clone(),
            settings_path.with_extension("backup.json"),
        ];
        if self.remote_root_uri.is_some() {
            let previous = crate::settings::project_settings_path(&self.shared.root)
                .parent()
                .unwrap()
                .join("completion-settings.json");
            settings_paths.extend([previous.clone(), previous.with_extension("backup.json")]);
        }
        for (position, settings_path) in settings_paths.iter().enumerate() {
            if let Ok(bytes) = read_bounded(settings_path, MAX_PREFERENCE_BYTES)
                && let Ok(settings) = serde_json::from_slice::<IndexSettings>(&bytes)
            {
                let mut control = self
                    .shared
                    .control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if !control.configured {
                    control.settings = settings.bounded();
                    if self.shared.buffer_only {
                        control.settings.project_wide = false;
                    }
                }
                self.preferences_dirty |= position >= 2;
                break;
            }
        }
        let settings = {
            let control = self
                .shared
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Configure may arrive before this thread finishes startup. It
            // still represents a user preference that must reach disk.
            self.preferences_dirty |= control.configured;
            control.settings.clone()
        };
        self.last_settings = settings.clone();
        self.restore_cache(&settings);
        if settings.enabled && settings.project_wide {
            self.begin_walk();
        }
        self.publish(IndexPhase::Loading, None, true);
    }

    fn restore_cache(&mut self, settings: &IndexSettings) {
        self.checkpoint_complete = true;
        let path = self.shared.cache_dir.join("completion-index.jsonl");
        if settings.enabled
            && let Ok(file) = std::fs::File::open(path)
        {
            let mut reader = std::io::BufReader::new(file);
            if let Ok(Some(line)) = read_cache_line(&mut reader, 64 * 1024)
                && let Ok(header) = serde_json::from_slice::<CacheHeader>(&line)
                && header.version == CACHE_VERSION
                && header.root == self.shared.root
                && header.remote_root_uri == self.remote_root_uri
            {
                self.checkpoint_complete = false;
                self.restore = Some(reader);
            }
        }
    }

    fn restore_next(&mut self, settings: &IndexSettings) -> Option<FileRecord> {
        let reader = self.restore.as_mut()?;
        // A corrupt cache cannot allocate an unbounded line or contribution.
        let line = match read_cache_line(reader, 2 * 1024 * 1024) {
            Ok(Some(line)) => line,
            Ok(None) => {
                self.restore = None;
                self.checkpoint_complete = true;
                return None;
            }
            Err(_) => {
                self.restore = None;
                self.checkpoint_complete = true;
                self.skipped += 1;
                return None;
            }
        };
        let record: FileRecord = match serde_json::from_slice(&line) {
            Ok(record) => record,
            Err(_) => {
                self.skipped += 1;
                return None;
            }
        };
        let valid = CompletionIndex::eligible(&self.shared, &record.path)
            && (!self.shared.buffer_only || !record.dirty)
            && record.path.as_os_str().len() <= MAX_CACHE_PATH_BYTES
            && record.terms.len() <= MAX_TERMS_PER_FILE
            && record.examples.valid()
            && record
                .stamp
                .as_ref()
                .is_some_and(|s| s.bytes <= settings.max_file_bytes as u64)
            && record.terms.iter().all(|term| {
                (2..=MAX_WORD_BYTES).contains(&term.word.len())
                    && term.word.chars().all(is_word)
                    && term.frequency > 0
                    && term.calls <= term.frequency
                    && term.domains.iter().all(|count| *count <= term.frequency)
                    && term.contexts.len() <= MAX_CONTEXTS
                    && term.contexts.iter().all(|FileContext(_, count, calls)| {
                        *count <= term.frequency && *calls <= *count
                    })
            });
        if valid {
            Some(record)
        } else {
            self.skipped += 1;
            None
        }
    }

    fn validate_next(&mut self, settings: &IndexSettings) -> bool {
        if let Some(path) = self.validation.pop_front() {
            self.publish(IndexPhase::Indexing, Some(path.clone()), false);
            self.index_disk(path, settings);
            return true;
        }
        if self.restore.is_none() {
            return false;
        }
        if let Some(record) = self.restore_next(settings) {
            self.discovered += 1;
            self.publish(IndexPhase::Indexing, Some(record.path.clone()), false);
            if self.shared.buffer_only {
                // Live buffers win over cached records, including observations
                // queued while this bounded restore was in progress.
                let observed = self
                    .shared
                    .control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pending
                    .contains_key(&record.path);
                if !observed && !self.files.contains_key(&record.path) {
                    self.install(record, settings);
                }
            } else if let Some(stamp) = Stamp::read(&record.path) {
                if Some(&stamp) == record.stamp.as_ref() && !self.files.contains_key(&record.path) {
                    self.install(record, settings);
                } else {
                    self.index_disk(record.path, settings);
                }
            } else {
                self.remove(&record.path);
            }
        }
        true
    }

    fn begin_walk(&mut self) {
        if self.shared.buffer_only {
            return;
        }
        self.watch_rules = WatchRules::default();
        let mut builder = ignore::WalkBuilder::new(&self.shared.root);
        builder
            .hidden(true)
            .require_git(false)
            .follow_links(false)
            .filter_entry(|entry| {
                !entry.file_type().is_some_and(|kind| kind.is_dir())
                    || !excluded_directory(entry.file_name())
            });
        self.walk = Some(builder.build());
        self.discovered = self.files.len();
        self.skipped = 0;
    }

    fn scan_step(&mut self, settings: &IndexSettings) {
        let started = Instant::now();
        for _ in 0..32 {
            let Some(entry) = self.walk.as_mut().and_then(Iterator::next) else {
                self.walk = None;
                return;
            };
            if let Ok(entry) = entry
                && entry.file_type().is_some_and(|kind| kind.is_file())
            {
                self.discovered += 1;
                self.publish(IndexPhase::Scanning, Some(entry.path().to_owned()), false);
                self.index_disk(entry.into_path(), settings);
                return;
            }
            if started.elapsed() >= Duration::from_millis(3) {
                return;
            }
        }
    }

    fn remove(&mut self, path: &Path) {
        self.recent.retain(|recent| recent != path);
        if let Some(old) = self.files.remove(path) {
            self.memory = self.memory.saturating_sub(old.memory());
            self.example_bytes = self.example_bytes.saturating_sub(old.examples.memory());
            self.recency
                .remove(crate::completion_source::Source::new(Some(path), "").file);
            let mut catalog = self
                .shared
                .catalog
                .write()
                .unwrap_or_else(|e| e.into_inner());
            catalog.remove_record(&old);
            self.shared
                .known
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(path);
            self.cache_dirty = true;
        }
    }

    #[cfg(test)]
    fn index_snapshot(&mut self, path: PathBuf, rope: ropey::Rope, settings: &IndexSettings) {
        self.queue_snapshot(path, rope, settings, false);
    }

    fn queue_snapshot(
        &mut self,
        path: PathBuf,
        rope: ropey::Rope,
        settings: &IndexSettings,
        dirty: bool,
    ) {
        if rope.len_bytes() > settings.max_file_bytes {
            self.skipped += 1;
            self.remove(&path);
            return;
        }
        self.scans.retain(|scan| scan.path != path);
        let scan = stream::Scan::snapshot(path, rope, settings.max_file_bytes, dirty);
        self.scans.push_front(scan);
    }

    fn index_disk(&mut self, path: PathBuf, settings: &IndexSettings) {
        if self.shared.buffer_only {
            return;
        }
        if self.files.get(&path).is_some_and(|old| old.dirty) {
            return;
        }
        let Some(stamp) = Stamp::read(&path) else {
            self.remove(&path);
            return;
        };
        if stamp.bytes > settings.max_file_bytes as u64 {
            self.skipped += 1;
            self.remove(&path);
            return;
        }
        if self
            .files
            .get(&path)
            .is_some_and(|old| !old.dirty && old.stamp.as_ref() == Some(&stamp))
        {
            {
                self.shared
                    .known
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(path);
            }
            return;
        }
        match stream::Scan::disk(path.clone(), stamp, settings.max_file_bytes) {
            Ok(mut scan) => {
                scan.demand = self.shared.active_file.load(Ordering::Acquire)
                    == crate::completion_source::Source::new(Some(&path), "").file;
                self.scans.retain(|scan| scan.path != path);
                self.scans.push_front(scan);
            }
            Err(_) => {
                self.skipped += 1;
                self.remove(&path);
            }
        }
    }

    fn touch_path(&mut self, path: &Path) {
        if self.files.contains_key(path) {
            self.recency
                .touch(crate::completion_source::Source::new(Some(path), "").file);
        }
    }

    fn index_opened_disk(&mut self, path: PathBuf, settings: &IndexSettings) {
        self.touch_path(&path);
        self.index_disk(path.clone(), settings);
        if let Some(scan) = self.scans.iter_mut().find(|scan| scan.path == path) {
            scan.demand = true;
        }
    }

    fn advance_scan(&mut self, settings: &IndexSettings) {
        if let Some(at) = self
            .scans
            .iter()
            .position(|scan| self.active_path.as_ref() == Some(&scan.path))
        {
            self.scans.swap(0, at);
        }
        if let Some(path) = self.scans.front().map(|scan| scan.path.clone()) {
            self.publish(IndexPhase::Indexing, Some(path), false);
        }
        let Some(mut scan) = self.scans.pop_front() else {
            return;
        };
        match scan.step() {
            Ok(false) => self.scans.push_front(scan),
            Ok(true) => {
                let busy = self
                    .shared
                    .control
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .activity
                    .remaining()
                    .is_some();
                if busy {
                    // A completed scan can be finalized on a later turn. Avoid
                    // dictionary publication/compaction during a new UI action.
                    self.scans.push_front(scan);
                    return;
                }
                let demand = scan.demand;
                let record = scan.finish();
                if let Some(old) = self.files.get_mut(&record.path)
                    && old.hash == record.hash
                {
                    let id = crate::completion_source::Source::new(Some(&record.path), "").file;
                    let mut catalog = self
                        .shared
                        .catalog
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    catalog.files.remove(&id);
                    let old_mut = Arc::make_mut(old);
                    old_mut.stamp = record.stamp;
                    old_mut.dirty = record.dirty;
                    catalog.files.insert(id, Arc::clone(old));
                    drop(catalog);
                    self.cache_dirty = true;
                    let path = record.path;
                    {
                        self.shared
                            .known
                            .write()
                            .unwrap_or_else(|error| error.into_inner())
                            .insert(path);
                    }
                } else {
                    self.install_with_policy(record, settings, demand);
                }
            }
            Err(_) => {
                self.skipped += 1;
                self.remove(&scan.path);
            }
        }
    }
    fn install(&mut self, record: FileRecord, settings: &IndexSettings) {
        self.install_with_policy(record, settings, false);
    }

    fn fits(&self, record: &FileRecord, settings: &IndexSettings) -> bool {
        let catalog = self
            .shared
            .catalog
            .read()
            .unwrap_or_else(|e| e.into_inner());
        self.memory
            + self.recency.memory()
            + self.recency.insertion_cost()
            + catalog.memory_bytes
            + 208
            + record.path.as_os_str().len() * 2
            + record.terms.len() * 20
            + record.examples.memory()
            + catalog.additional_memory(&record.terms)
            + catalog.examples.additional_memory(&record.examples)
            <= retained_limit(settings)
    }

    fn install_with_policy(
        &mut self,
        mut record: FileRecord,
        settings: &IndexSettings,
        demand: bool,
    ) {
        if record
            .terms
            .windows(2)
            .any(|pair| pair[0].word >= pair[1].word)
        {
            let terms = Arc::make_mut(&mut record.terms);
            terms.sort_unstable_by(|a, b| a.word.cmp(&b.word));
            terms.dedup_by(|a, b| a.word == b.word);
        }
        Arc::make_mut(&mut record.examples).truncate_to(Catalog::retained_tokens());
        record.cache_memory();
        self.remove(&record.path);
        if !demand && self.files.len() < settings.max_files {
            for _ in 0..32 {
                if self.fits(&record, settings) {
                    break;
                }
                if !self.compact_one() {
                    break;
                }
            }
            while !self.fits(&record, settings) && record.reducible_memory() > 0 {
                record.compact();
            }
        }
        if self.files.len() >= settings.max_files || !self.fits(&record, settings) {
            if demand {
                // One parsed candidate stays in the existing working reserve.
                // Reclamation is resumed between worker turns, never on input.
                let empty = Catalog::default();
                let alone = |record: &FileRecord| {
                    640 + 208
                        + record.path.as_os_str().len() * 2
                        + record.terms.len() * 20
                        + record.examples.memory()
                        + empty.additional_memory(&record.terms)
                        + empty.examples.additional_memory(&record.examples)
                        <= retained_limit(settings)
                };
                if !alone(&record) {
                    record.compact();
                }
                if alone(&record) {
                    self.admission = Some(record);
                    self.limited = false;
                    return;
                }
            }
            self.skipped += 1;
            self.limited = true;
            self.walk = None;
            self.restore = None;
            self.checkpoint_complete = true;
            return;
        }
        self.commit_record(record, settings);
    }

    fn advance_admission(&mut self, settings: &IndexSettings) {
        let Some(record) = self.admission.take() else {
            return;
        };
        let started = Instant::now();
        let activity = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .activity
            .clone();
        for _ in 0..32 {
            if activity.remaining().is_some() {
                self.admission = Some(record);
                return;
            }
            if self.files.len() < settings.max_files && self.fits(&record, settings) {
                self.limited = false;
                self.commit_record(record, settings);
                return;
            }
            let active = self.shared.active_file.load(Ordering::Acquire);
            let incoming = crate::completion_source::Source::new(Some(&record.path), "").file;
            let victim = self.recency.victim(active, incoming).and_then(|id| {
                self.shared
                    .catalog
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .files
                    .get(&id)
                    .map(|stored| stored.path.clone())
            });
            let Some(path) = victim else {
                self.skipped += 1;
                // A background open cannot evict the currently active file.
                return;
            };
            self.remove(&path);
            self.evicted = self.evicted.saturating_add(1);
            if started.elapsed() >= Duration::from_millis(3) {
                break;
            }
        }
        self.admission = Some(record);
    }

    fn commit_record(&mut self, record: FileRecord, settings: &IndexSettings) {
        let path = record.path.clone();
        let stored = {
            let mut catalog = self
                .shared
                .catalog
                .write()
                .unwrap_or_else(|e| e.into_inner());
            catalog.pack(record, settings)
        };
        self.memory += stored.memory();
        self.example_bytes += stored.examples.memory();
        self.recency
            .touch(crate::completion_source::Source::new(Some(&path), "").file);
        self.files.insert(path.clone(), stored);
        self.recent.retain(|other| other != &path);
        self.recent.push_front(path.clone());
        self.recent.truncate(12);
        self.shared
            .known
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(path.clone());
        // Rich examples have their own ceiling; spare memory is not a request
        // to retain every available n-gram until the entire workspace fills up.
        for _ in 0..32 {
            if self
                .shared
                .catalog
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .examples
                .memory()
                + self.example_bytes
                <= Catalog::example_budget(settings)
            {
                break;
            }
            if !self.compact_example_one() {
                break;
            }
        }
        while self
            .shared
            .catalog
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .examples
            .memory()
            + self.example_bytes
            > Catalog::example_budget(settings)
            && self
                .files
                .get(&path)
                .is_some_and(|r| r.examples.tokens() > 8)
        {
            self.compact_path(&path, true);
        }
        self.enforce_limits(settings);
        self.cache_dirty = true;
    }

    fn compact_path(&mut self, path: &Path, examples_only: bool) {
        let Some(record) = self.files.get_mut(path) else {
            return;
        };
        let before = record.memory();
        let old_examples = record.examples.memory();
        let file = crate::completion_source::Source::new(Some(path), "").file;
        let mut catalog = self
            .shared
            .catalog
            .write()
            .unwrap_or_else(|e| e.into_inner());
        catalog.files.remove(&file);
        catalog.examples.remove(file);
        let record_mut = Arc::make_mut(record);
        if !examples_only && record_mut.terms.context_bytes() > 0 {
            catalog.drop_contexts(&record_mut.terms);
            record_mut.terms = Arc::new(record_mut.terms.without_contexts());
        }
        Arc::make_mut(&mut record_mut.examples).compact();
        self.memory = self.memory.saturating_sub(before) + record_mut.memory();
        self.example_bytes =
            self.example_bytes.saturating_sub(old_examples) + record_mut.examples.memory();
        catalog
            .examples
            .insert(file, Arc::clone(&record_mut.examples));
        catalog.files.insert(file, Arc::clone(record));
        catalog.refresh_memory();
        self.cache_dirty = true;
    }

    fn compact_next(&mut self, examples_only: bool) -> bool {
        use std::ops::Bound::{Excluded, Included, Unbounded};
        let candidate = self
            .files
            .range::<Path, _>((Excluded(self.compact_after.as_path()), Unbounded))
            .chain(
                self.files
                    .range::<Path, _>((Unbounded, Included(self.compact_after.as_path()))),
            )
            .find(|(_, r)| {
                if examples_only {
                    r.examples.tokens() > 8
                } else {
                    r.reducible_memory() > 0
                }
            })
            .map(|(path, _)| path.clone());
        let Some(path) = candidate else { return false };
        self.compact_after.clone_from(&path);
        self.compact_path(&path, examples_only);
        true
    }
    fn compact_one(&mut self) -> bool {
        self.compact_next(false)
    }
    fn compact_example_one(&mut self) -> bool {
        self.compact_next(true)
    }

    fn trim_oversized(&mut self, settings: &IndexSettings) {
        use std::ops::Bound::{Excluded, Unbounded};
        for _ in 0..16 {
            let Some(cursor) = self.oversized_after.as_ref() else {
                break;
            };
            let next = self
                .files
                .range::<Path, _>((Excluded(cursor.as_path()), Unbounded))
                .next()
                .map(|(path, record)| {
                    (
                        path.clone(),
                        record
                            .stamp
                            .as_ref()
                            .is_some_and(|stamp| stamp.bytes > settings.max_file_bytes as u64),
                    )
                });
            let Some((path, oversized)) = next else {
                self.oversized_after = None;
                break;
            };
            self.oversized_after = Some(path.clone());
            if oversized {
                self.remove(&path);
            }
        }
    }

    fn enforce_limits(&mut self, settings: &IndexSettings) {
        let mut compactions = 0;
        while self.files.len() > settings.max_files
            || self.memory
                + self.recency.memory()
                + self
                    .shared
                    .catalog
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .memory_bytes
                > retained_limit(settings)
        {
            if self.files.len() <= settings.max_files && compactions < 16 && self.compact_one() {
                compactions += 1;
                continue;
            }
            let active = self.shared.active_file.load(Ordering::Acquire);
            let victim = self
                .recency
                .victim(active, 0)
                .or_else(|| self.recency.victim(0, 0));
            let path = victim.and_then(|id| {
                self.shared
                    .catalog
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .files
                    .get(&id)
                    .map(|record| record.path.clone())
            });
            let Some(path) = path else {
                break;
            };
            self.remove(&path);
            self.evicted = self.evicted.saturating_add(1);
            self.limited = true;
        }
    }

    fn publish(&mut self, phase: IndexPhase, current: Option<PathBuf>, force: bool) {
        if !force && self.last_publish.elapsed() < Duration::from_millis(100) {
            return;
        }
        let control = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        status.indexed_files = self.files.len();
        status.pending_files = control.pending.len()
            + usize::from(self.admission.is_some())
            + usize::from(self.oversized_after.is_some())
            + self.scans.len()
            + self.validation.len()
            + usize::from(self.restore.is_some());
        status.discovered_files = self.discovered.max(self.files.len());
        status.skipped_files = self.skipped;
        status.evicted_files = self.evicted;
        status.current_file = current.or_else(|| {
            matches!(phase, IndexPhase::Indexing | IndexPhase::Scanning)
                .then(|| self.scans.front().map(|scan| scan.path.clone()))
                .flatten()
        });
        (status.processed_bytes, status.current_file_bytes) =
            self.scans.front().map_or((0, 0), stream::Scan::progress);
        status.recent_files = self.recent.iter().cloned().collect();
        status.phase = phase;
        status.settings = control.settings.clone();
        status.memory_bytes = self.memory
            + self.recency.memory()
            + self
                .shared
                .catalog
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .memory_bytes
            + if !self.scans.is_empty() || self.restore.is_some() || self.admission.is_some() {
                LEARNING_WORKING_RESERVE
            } else {
                0
            };
        drop(status);
        drop(control);
        self.last_publish = Instant::now();
        self.shared.revision.fetch_add(1, Ordering::Release);
        self.shared
            .wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .notify();
    }

    fn error(&self, error: std::io::Error) {
        self.shared
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error = Some(error.to_string());
    }

    fn save_preferences(&mut self, settings: &IndexSettings) {
        if let Err(error) = write_json(
            &self.shared.cache_dir.join("completion-settings.json"),
            settings,
        ) {
            self.error(error);
        }
        self.preferences_dirty = false;
    }

    fn pending_save_wait(&self) -> Option<Duration> {
        if !self.cache_dirty && !self.history_dirty {
            return None;
        }
        let control = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (!control.stopped)
            .then(|| control.activity.maintenance_remaining())
            .flatten()
    }

    fn save(&mut self) {
        self.erase_pending();
        if self.shared.buffer_only {
            // Accepted-choice history remains session-scoped in remote mode.
            self.history_dirty = false;
        }
        // Do not repeatedly serialize large checkpoints between keystrokes or
        // scroll samples. One real deadline persists dirty data after quiet;
        // shutdown bypasses this delay, and no timer exists for a clean cache.
        if self.pending_save_wait().is_some() {
            return;
        }
        if self.history_dirty {
            let history = self
                .shared
                .catalog
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .history
                .clone();
            match write_json(
                &self.shared.cache_dir.join("completion-history.json"),
                &history,
            ) {
                Ok(()) => self.history_dirty = false,
                Err(error) => {
                    self.error(error);
                    self.history_dirty = false;
                }
            }
        }
        if !self.cache_dirty || self.restore.is_some() || !self.checkpoint_complete {
            return;
        }
        // Serialize a single coalesced checkpoint after a burst or each 3s,
        // never once per indexed file. Unsaved buffer contents stay in memory.
        let path = self.shared.cache_dir.join("completion-index.jsonl");
        let result = (|| -> std::io::Result<()> {
            use std::io::Write;
            std::fs::create_dir_all(&self.shared.cache_dir)?;
            let temporary = temporary_path(&path);
            {
                let mut writer = std::io::BufWriter::new(std::fs::File::create(&temporary)?);
                serde_json::to_writer(
                    &mut writer,
                    &CacheHeader {
                        version: CACHE_VERSION,
                        root: self.shared.root.clone(),
                        remote_root_uri: self.remote_root_uri.clone(),
                    },
                )?;
                writer.write_all(b"\n")?;
                for record in self.files.values().filter(|record| !record.dirty) {
                    let catalog = self
                        .shared
                        .catalog
                        .read()
                        .unwrap_or_else(|e| e.into_inner());
                    let decoded = record.decoded(&catalog);
                    drop(catalog);
                    serde_json::to_writer(&mut writer, &decoded)?;
                    writer.write_all(b"\n")?;
                }
                writer.flush()?;
            }
            replace_file(&temporary, &path)
        })();
        match result {
            Ok(()) => {
                self.cache_dirty = false;
            }
            Err(error) => {
                self.error(error);
            }
        }
        self.last_save = Instant::now();
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temporary = temporary_path(path);
    {
        let mut writer = std::io::BufWriter::new(std::fs::File::create(&temporary)?);
        serde_json::to_writer(&mut writer, value)?;
        std::io::Write::flush(&mut writer)?;
    }
    replace_file(&temporary, path)
}

/// The file may grow after its metadata was checked. Enforce the bound on
/// the reader itself, including saved-buffer comparison and preferences.
fn read_bounded(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Completion file exceeds size limit",
        ));
    }
    Ok(bytes)
}

fn read_cache_line(
    reader: &mut impl std::io::BufRead,
    limit: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::BufRead;
    let mut bytes = Vec::new();
    std::io::Read::take(reader, limit as u64 + 1).read_until(b'\n', &mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::other(
            "Completion cache record exceeds limit",
        ));
    }
    Ok((!bytes.is_empty()).then_some(bytes))
}

fn atomic_replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    // MoveFileEx replaces the destination on Windows; rename is atomic on Unix.
    #[cfg(windows)]
    let moved = {
        use std::os::windows::ffi::OsStrExt;
        let source: Vec<_> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            use windows_sys::Win32::Storage::FileSystem::{
                MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
            };
            if MoveFileExW(
                source.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            ) == 0
            {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    };
    #[cfg(not(windows))]
    let moved = std::fs::rename(temporary, path);
    moved
}

fn replace_file(temporary: &Path, path: &Path) -> std::io::Result<()> {
    match atomic_replace_file(temporary, path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            // Some sandbox filesystems permit file writes but reject moving
            // directory entries. The index is a validated disposable cache;
            // preserve a recoverable preference copy before this fallback.
            if path
                .file_name()
                .is_some_and(|name| name == "completion-settings.json")
            {
                let previous_valid = read_bounded(path, MAX_PREFERENCE_BYTES)
                    .ok()
                    .is_some_and(|bytes| serde_json::from_slice::<IndexSettings>(&bytes).is_ok());
                std::fs::copy(
                    if previous_valid { path } else { temporary },
                    path.with_extension("backup.json"),
                )?;
            }
            std::fs::copy(temporary, path)?;
            let _ = std::fs::remove_file(temporary);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(name)
}

#[cfg(test)]
#[path = "completion_index_tests.rs"]
mod tests;
