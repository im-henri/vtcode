//! Conditional clipboard edits without network waits on the input thread.
//! A shared chunk log makes worker snapshots constant-time. Resolution replays
//! pure edits on a worker, using their original timestamps and undo groups.
use super::*;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

const MAX_EVENTS: usize = 4096;
const MAX_BYTES: usize = 1024 * 1024;
const CHUNK_EVENTS: usize = 64;

#[derive(Clone)]
struct Event {
    code: KeyCode,
    modifiers: KeyModifiers,
    paste: Option<Arc<str>>,
    conditional: Option<u64>,
    when: Instant,
    multiline: bool,
    accept: fn(char) -> bool,
}
struct Chunk {
    previous: Option<Arc<Chunk>>,
    events: Vec<Event>,
}
#[derive(Clone, Default)]
struct Events {
    tail: Option<Arc<Chunk>>,
    len: usize,
    bytes: usize,
}
impl Events {
    fn push(&mut self, event: Event) -> bool {
        let bytes = event.paste.as_ref().map_or(0, |value| value.len());
        if self.len == MAX_EVENTS || self.bytes.saturating_add(bytes) > MAX_BYTES {
            return false;
        }
        self.len += 1;
        self.bytes += bytes;
        if let Some(chunk) = self.tail.as_mut().and_then(Arc::get_mut)
            && chunk.events.len() < CHUNK_EVENTS
        {
            chunk.events.push(event);
        } else {
            self.tail = Some(Arc::new(Chunk {
                previous: self.tail.take(),
                events: vec![event],
            }));
        }
        true
    }
    fn chunks(&self) -> Vec<&Chunk> {
        let mut chunks = Vec::new();
        let mut next = self.tail.as_deref();
        while let Some(chunk) = next {
            chunks.push(chunk);
            next = chunk.previous.as_deref();
        }
        chunks.reverse();
        chunks
    }
}
impl Drop for Events {
    fn drop(&mut self) {
        // Frequent snapshots can leave one event in each shared chunk. Drop a
        // long unique tail iteratively instead of recursing through Arc links.
        let mut next = self.tail.take();
        while let Some(chunk) = next {
            match Arc::try_unwrap(chunk) {
                Ok(mut chunk) => next = chunk.previous.take(),
                Err(_) => break,
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct DeferredInput {
    initial: Arc<Snapshot>,
    history: Arc<TextInputHistory>,
    events: Events,
    latest: Arc<AtomicU64>,
    id: u64,
}
impl std::fmt::Debug for DeferredInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeferredInput")
            .field("id", &self.id)
            .field("events", &self.events.len)
            .finish()
    }
}
pub(crate) struct ResolvedInput {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub history: TextInputHistory,
}
pub(crate) struct FrozenInput {
    history: TextInputHistory,
    pub replay: DeferredInput,
}
impl std::fmt::Debug for FrozenInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.replay.fmt(f)
    }
}

impl DeferredInput {
    pub(super) fn can_start(before: &Snapshot) -> bool {
        before.0.len() <= MAX_BYTES
    }
    pub(super) fn new(history: TextInputHistory, initial: Snapshot, id: u64) -> Self {
        debug_assert!(history.deferred.is_none());
        Self {
            initial: Arc::new(initial),
            history: Arc::new(history),
            events: Events::default(),
            latest: Arc::default(),
            id,
        }
    }
    pub(crate) fn stamp(&self) -> (u64, usize) {
        (self.id, self.events.len)
    }
    pub(super) fn invalidate(&self) {
        self.latest.fetch_add(1, Ordering::Release);
    }
    pub(super) fn record(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        paste: Option<&str>,
        conditional: Option<u64>,
        when: Instant,
        multiline: bool,
        accept: fn(char) -> bool,
    ) -> bool {
        if !self.events.push(Event {
            code,
            modifiers,
            paste: paste.map(Arc::from),
            conditional,
            when,
            multiline,
            accept,
        }) {
            return false;
        }
        self.latest.store(self.events.len as u64, Ordering::Release);
        true
    }
    pub(crate) fn candidates(&self) -> Vec<(u64, Arc<str>)> {
        self.events
            .chunks()
            .into_iter()
            .flat_map(|chunk| &chunk.events)
            .filter_map(|event| Some((event.conditional?, event.paste.clone()?)))
            .collect()
    }
    /// No clipboard access or layout work is allowed during replay. A newer
    /// keystroke invalidates this snapshot; the caller reuses classification
    /// results and schedules the newest local snapshot instead of another RPC.
    pub(crate) fn resolve(
        &self,
        decisions: &HashMap<u64, bool>,
        cancel: &crate::background::CancellationToken,
    ) -> Option<ResolvedInput> {
        let current = || {
            !cancel.is_cancelled() && self.latest.load(Ordering::Acquire) == self.events.len as u64
        };
        if !current() {
            return None;
        }
        let (mut text, mut cursor, mut anchor) = (*self.initial).clone();
        let mut history = (*self.history).clone();
        for chunk in self.events.chunks() {
            for event in &chunk.events {
                if !current() {
                    return None;
                }
                if event
                    .conditional
                    .is_some_and(|id| decisions.get(&id) == Some(&true))
                {
                    continue;
                }
                history.edit_at(
                    TextInput::new(&mut text, &mut cursor, &mut anchor)
                        .multiline(event.multiline)
                        .filter(event.accept),
                    event.code,
                    event.modifiers,
                    event.paste.as_deref(),
                    None,
                    event.when,
                );
            }
        }
        current().then_some(ResolvedInput {
            text,
            cursor,
            anchor,
            history,
        })
    }
}
impl FrozenInput {
    pub(crate) fn restore(&self) -> ResolvedInput {
        let mut history = self.history.clone();
        let (text, cursor, anchor) = history.current.clone().expect("frozen input snapshot");
        history.deferred = Some(Box::new(self.replay.clone()));
        ResolvedInput {
            text,
            cursor,
            anchor,
            history,
        }
    }
}

impl TextInputHistory {
    pub(crate) fn discard_deferred(&mut self) {
        if let Some(replay) = self.deferred.take() {
            replay.invalidate();
        }
    }
    fn matches(&self, text: &str, cursor: usize, anchor: Option<usize>) -> bool {
        self.current.as_ref().is_some_and(|previous| {
            previous.0 == text && previous.1 == cursor && previous.2 == anchor
        })
    }
    pub(crate) fn deferred_input(
        &self,
        text: &str,
        cursor: usize,
        anchor: Option<usize>,
    ) -> Option<&DeferredInput> {
        let replay = self.deferred.as_deref()?;
        self.matches(text, cursor, anchor).then_some(replay)
    }
    pub(crate) fn deferred_stamp(&self) -> Option<(u64, usize)> {
        self.deferred.as_deref().map(DeferredInput::stamp)
    }
    pub(crate) fn freeze(
        mut self,
        text: &str,
        cursor: usize,
        anchor: Option<usize>,
    ) -> Option<FrozenInput> {
        if !self.matches(text, cursor, anchor) {
            return None;
        }
        Some(FrozenInput {
            replay: *self.deferred.take()?,
            history: self,
        })
    }
    pub(crate) fn apply_resolved(
        &mut self,
        input: TextInput<'_>,
        stamp: (u64, usize),
        mut resolved: ResolvedInput,
    ) -> bool {
        if self
            .deferred_input(input.text, *input.cursor, *input.selection_anchor)
            .is_none_or(|replay| replay.stamp() != stamp)
        {
            return false;
        }
        resolved.history.revision = self.revision.wrapping_add(1);
        *input.text = resolved.text;
        *input.cursor = resolved.cursor;
        *input.selection_anchor = resolved.anchor;
        *self = resolved.history;
        true
    }
}

pub(super) fn clipboard_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    let primary = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && !modifiers.contains(KeyModifiers::ALT);
    (primary
        && matches!(
            code,
            KeyCode::Char('c' | 'C' | 'x' | 'X' | 'v' | 'V') | KeyCode::Insert
        ))
        || (modifiers.contains(KeyModifiers::SHIFT)
            && (code == KeyCode::Insert || (!primary && code == KeyCode::Delete)))
}

#[cfg(test)]
#[path = "deferred_input_tests.rs"]
mod tests;
