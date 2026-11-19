//! Offline content classification using Magika's embedded, trained model.
//! Snapshotting, feature extraction and inference have fixed bounds independent
//! of document size. Model initialization and inference run only on workers.
use crate::background::CancellationToken;
use ropey::Rope;
use std::{
    cell::RefCell,
    hash::Hasher,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

// Magika's pinned standard_v3_3 implementation reads 4096 bytes from either end,
// strips whitespace, then feeds 1024 bytes from each end to its classifier.
// Preserve those blocks without copying the middle of a large document.
pub(crate) const SAMPLE_BLOCK_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Binding {
    pub syntax: String,
    pub path: Option<std::path::PathBuf>,
    pub registry: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Observation {
    pub epoch: u64,
    pub revision: u64,
    pub version: u64,
    pub path: Option<std::path::PathBuf>,
    pub registry: u64,
    pub manual: Option<String>,
    pub known: bool,
}

/// Owned, trimmed ropes cannot keep a large, obsolete document alive while
/// work is queued. At most 8198 bytes are retained (including UTF-8 boundaries).
pub(crate) struct Snapshot {
    head: Rope,
    tail: Rope,
    tail_skip: usize,
    len: usize,
}
impl Snapshot {
    pub(crate) fn capture(rope: &Rope) -> Self {
        let len = rope.len_bytes();
        let end_byte = len.min(SAMPLE_BLOCK_BYTES);
        let mut end = rope.byte_to_char(end_byte);
        if rope.char_to_byte(end) < end_byte {
            end += 1;
        }
        let start_byte = len.saturating_sub(SAMPLE_BLOCK_BYTES).max(end_byte);
        let start = rope.byte_to_char(start_byte);
        Self {
            head: Rope::from(rope.slice(..end)),
            tail: Rope::from(rope.slice(start..)),
            tail_skip: start_byte - rope.char_to_byte(start),
            len,
        }
    }

    pub(crate) fn sample(&self, cancel: &CancellationToken) -> Option<Sample> {
        if cancel.is_cancelled() {
            return None;
        }
        // Magika may read a small input in full. Pass a complete byte slice
        // containing both feature blocks, with any overlap included once.
        let mut bytes = Vec::with_capacity(self.len.min(SAMPLE_BLOCK_BYTES * 2));
        bytes.extend(self.head.bytes().take(SAMPLE_BLOCK_BYTES));
        bytes.extend(self.tail.bytes().skip(self.tail_skip));
        (!cancel.is_cancelled()).then_some(Sample {
            bytes,
            original_len: self.len,
        })
    }

    #[cfg(test)]
    pub(crate) fn retained_bytes(&self) -> usize {
        self.head.len_bytes() + self.tail.len_bytes()
    }
}

pub(crate) struct Sample {
    bytes: Vec<u8>,
    original_len: usize,
}
impl Sample {
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut hash = rustc_hash::FxHasher::default();
        hash.write_usize(self.original_len);
        hash.write(&self.bytes);
        hash.finish()
    }

    pub(crate) fn first_line(&self) -> std::borrow::Cow<'_, str> {
        // Existing grammar first-line rules and modelines can cover languages
        // outside the model. Bound even an enormous one-line document.
        let line = self.bytes[..self.bytes.len().min(256)]
            .split(|&byte| byte == b'\n')
            .next()
            .unwrap_or_default();
        String::from_utf8_lossy(line)
    }
}

// One lazily loaded CPU model per process, shared across workers.
// Magika's CPU runtime explicitly uses Tract's SingleThread executor on the
// calling worker. No spinning inference pool, native ONNX installation, GPU
// initialization or runtime download is needed.
static MODEL: LazyLock<Option<magika::Runtime>> = LazyLock::new(|| {
    let runtime = magika::Runtime::builder()
        .with_backend(magika::Backend::Cpu)
        .with_max_batch(1)
        .with_options(options(false))
        .build();
    match runtime {
        Ok(runtime) => Some(runtime),
        Err(error) => {
            log_warn!("content language detection unavailable: {error}");
            None
        }
    }
});

// Magika's execution state is explicitly thread-private. Keep one session per
// existing analysis worker, sharing the model's weights and prepared plan.
// The gate bounds total inference concurrency even across workspace switches.
thread_local! {
    static SESSION: RefCell<Option<magika::Session>> = const { RefCell::new(None) };
}
static INFERENCE: Mutex<()> = Mutex::new(());
static FAILED: AtomicBool = AtomicBool::new(false);

fn options(use_resolvers: bool) -> magika::Options {
    let mut options = magika::Options::default();
    // Resolvers validate whole documents on the short-input path. Longer
    // documents use the model's fixed beginning/end feature extraction.
    options.use_resolvers = use_resolvers;
    options
}

pub(crate) fn classify(
    sample: &Sample,
    cancel: &CancellationToken,
) -> Option<&'static magika::TypeInfo> {
    if cancel.is_cancelled() || sample.bytes.contains(&0) {
        return None;
    }
    // Strict validation can recognize short JSON containers that do not give
    // a statistical classifier enough evidence. Inspect the complete document
    // only when it fits in the sample, without allocating a parsed object tree.
    let use_resolvers = sample.original_len <= SAMPLE_BLOCK_BYTES;
    if use_resolvers && sample.bytes.len() >= 8 {
        if let Ok(text) = std::str::from_utf8(&sample.bytes) {
            let text = text.trim_start();
            if text.starts_with(['{', '[']) {
                let mut parser = serde_json::Deserializer::from_str(text);
                if <serde::de::IgnoredAny as serde::Deserialize>::deserialize(&mut parser).is_ok()
                    && parser.end().is_ok()
                {
                    return Some(magika::ContentType::Json.info());
                }
            }
        }
    }
    // Empty/very short documents don't even initialize the model.
    let options = options(use_resolvers);
    let features =
        match magika::FeaturesOr::extract_content(sample.bytes.as_slice(), &options).ok()? {
            magika::FeaturesOr::Features(features) => features,
            magika::FeaturesOr::Ruled(result) => {
                let info = result.info();
                return (info.is_text && info.label != "txt").then_some(info);
            }
        };
    if cancel.is_cancelled() {
        return None;
    }
    let _gate = INFERENCE.lock().unwrap_or_else(|error| error.into_inner());
    if cancel.is_cancelled() || FAILED.load(Ordering::Relaxed) {
        return None;
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(runtime) = MODEL.as_ref() else {
            return Ok(None);
        };
        SESSION.with_borrow_mut(|session| -> anyhow::Result<_> {
            if session.is_none() {
                *session = Some(runtime.session()?);
            }
            let session = session.as_mut().unwrap();
            session.options_mut().use_resolvers = options.use_resolvers;
            let result = session.identify_features(&features)?;
            // info() applies Magika's per-type confidence thresholds and
            // canonical mappings. Don't use its raw best guess.
            let info = result.info();
            Ok((info.is_text && info.label != "txt").then_some(info))
        })
    }));
    let info = match result {
        Ok(Ok(info)) => info,
        Ok(Err(error)) => {
            // Don't repeatedly retry a broken runtime on every edit.
            log_warn!("content language detection failed: {error}");
            FAILED.store(true, Ordering::Relaxed);
            return None;
        }
        Err(_) => {
            log_warn!("content language detection failed; disabled for this process");
            FAILED.store(true, Ordering::Relaxed);
            return None;
        }
    };
    if cancel.is_cancelled() {
        return None;
    }
    info
}

#[cfg(test)]
#[path = "language_detection_tests.rs"]
mod tests;
